//! Deterministic licensed external-pack manifest compiler.
//!
//! Scans a local pack folder, never the network: refuse symlink escapes and
//! special files, canonicalize stable relative paths, stream-hash regular
//! files, measure PNG/JPEG/WAV/MP4/GLB metadata, and emit a canonical
//! [`SourceCollection`] plus [`ImportManifest`] plus a machine-readable local
//! upload plan. Rights are an explicit source-only contract — never CC0 by
//! default, never inferred from a filename. Reruns over identical bytes and
//! metadata are byte-identical.

use crate::glb::inspect_glb;
use crate::stateful_billboard::StatefulBillboard;
use crate::world_nav::WorldNav;
use crate::thumbs::{jpeg_dims, parse_wav, png_dims, thumbnail_is_placeholder};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use crate::videothumb::probe_video;
use makepad_asset_client::json::{self, obj, s, Value};
use makepad_asset_client::util::from_hex_exact;
use makepad_asset_data::limits::{
    MAX_CLIPS, MAX_DOCUMENT_BYTES, MAX_FILE_BYTES, MAX_IMPORT_ASSETS, MAX_JOINTS, MAX_LICENSE_BYTES,
    MAX_LICENSE_REVISION_BYTES, MAX_NAME_BYTES, MAX_PACK_PATH_BYTES, MAX_PACK_VERSION_BYTES,
    MAX_STRING_BYTES, MAX_TEXTURE_DIM, MAX_TRIANGLES, MAX_VERTICES, THUMBNAIL_MIN_DIM,
};
use makepad_asset_data::{
    sha256, Anchor, AssetAlias, AssetFile, AssetKind, Axis, BlobId, Bounds, Capabilities, CoordinateSystem,
    DerivativePolicy, DeviceTier, FileRole, ImageDims, ImportAsset, ImportFile, ImportManifest,
    ImportThumbnail, MediaType, Metrics, PackEntryKey, Pivot, Quat, Redistribution, Rights,
    SourceCollection, SourceCollectionId, SourceOrigin, ThumbnailCells, ThumbnailMedia,
    ThumbnailMeta, ThumbnailView, ThumbnailViewKind, Transform, Vec3, IMPORT_ASSET_ID_POLICY_V1,
};
use makepad_render::skin::SkinnedModel;
use makepad_render::StaticModel;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
#[cfg(any(test, target_os = "macos", target_os = "linux"))]
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::io::Write;
use std::path::{Component, Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::time::SystemTime;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

/// Stable leaf names written into `--out`.
pub const SOURCE_COLLECTION_FILE: &str = "source_collection.canon";
pub const IMPORT_MANIFEST_FILE: &str = "import_manifest.canon";
pub const UPLOAD_PLAN_FILE: &str = "upload_plan.json";
const PLAN_SCHEMA: &str = "makepad-asset-importer-pack-upload-plan-v1";

const HASH_CHUNK: usize = 64 * 1024;
const MAX_SOURCE_CONFIG_BYTES: u64 = MAX_DOCUMENT_BYTES as u64;
const MAX_WALK_DEPTH: usize = 16;
const MAX_WALK_DIRS: usize = 4096;
const MAX_WALK_ENTRIES: usize = 8192;
/// Entries the walk will collect from ONE directory. A defensive bound on
/// `list_dir_bounded`'s `names` vector — the whole tree is already bounded
/// by `MAX_WALK_ENTRIES`, so this only decides how flat a pack may be.
///
/// It is NOT a content-contract number: nothing here is encoded, digested,
/// or compared against a golden. The contract's own shape is much wider —
/// `MAX_IMPORT_ASSETS` (1024) assets per pack at up to
/// `MAX_IMPORT_FILES_PER_ASSET` (32) files each.
///
/// At 1024 this bound contradicted the packs it serves. Vendors ship flat
/// kits — every model in one folder — and a STAGED model is five directory
/// entries that all share one entry key: payload, thumbnail, and the
/// `.aomesh` / `.ao.png` / `.shadowsdf` the AO bake writes beside it. So
/// 1024 entries meant ~204 models, and Kenney's brick-kit (296 models,
/// 1480 entries) and nature-kit (329, ~1645) could not be imported at all.
///
/// Sharding such a pack into subdirectories is NOT the alternative: a
/// directory segment is part of the entry key, and the key is the published
/// alias (`{source_id}/{pack_name}/{key}`), so sharding renames every asset
/// in the pack — see
/// `a_directory_segment_becomes_part_of_the_entry_key_and_the_alias`.
///
/// 4096 matches `MAX_WALK_DIRS`, clears the largest real kit better than
/// twice over, and stays strictly under `MAX_WALK_ENTRIES` so a pack that
/// is flat AND huge still meets a per-directory refusal it can read.
const MAX_DIR_ENTRIES: usize = 4096;
const MAX_GLB_CHUNKS: usize = 8;
const MAX_GLB_NODES: usize = 4096;
const MAX_GLB_MESHES: usize = 1024;
const MAX_GLB_PRIMITIVES: usize = 2048;
const MAX_GLB_ACCESSORS: usize = 32_768;
const MAX_GLB_BUFFER_VIEWS: usize = 32_768;
const MAX_GLB_NODE_DEPTH: usize = 64;
const MAX_GLB_CHILDREN_PER_NODE: usize = 256;
const MAX_MP4_BOX_DEPTH: u32 = 8;
const STAGING_PREFIX: &str = ".pack-import-staging-";
const UPLOADER_REVERIFY: &str =
    "re-hash each local_path; refuse the plan unless sha256(local_path) equals blob";

/// Root-anchored pack walk is ABI-validated only on macOS and Linux.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const PACK_IMPORT_NATIVE_WALK: bool = true;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const PACK_IMPORT_NATIVE_WALK: bool = false;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn pack_os_unsupported() -> PackImportError {
    #[cfg(windows)]
    {
        PackImportError::new(
            PackImportErrorKind::Io,
            "Windows is not supported for --import-pack; root-anchored openat is unavailable",
        )
    }
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
    {
        PackImportError::new(
            PackImportErrorKind::Io,
            "pack import is fail-closed on this Unix; only macOS and Linux have a validated native filesystem walk",
        )
    }
    #[cfg(not(any(unix, windows)))]
    {
        PackImportError::new(
            PackImportErrorKind::Io,
            "pack import is supported only on macOS and Linux",
        )
    }
}

/// glTF / Kenney fixture convention used by the content crate's pack tests.
const PACK_COORD: CoordinateSystem = CoordinateSystem {
    units_per_meter: 1.0,
    up: Axis::YPos,
    forward: Axis::ZNeg,
    pivot: Pivot::Origin,
};

/// Kenney collection identity used by `--import-pack` tests and the AI
/// Content Import page. Rights are the explicit CC-BY-4.0 grant already
/// pinned in this crate — never inferred, never CC0 by default.
pub const KENNEY_SOURCE_ID: &str = "kenney";
pub const KENNEY_SOURCE_TITLE: &str = "Kenney game assets";
pub const KENNEY_LICENSE: &str = "CC-BY-4.0";
pub const KENNEY_TERMS_URL: &str = "https://creativecommons.org/licenses/by/4.0/";
pub const KENNEY_CREDITS: &str = "Kenney (kenney.nl)";
pub const KENNEY_HOME: &str = "https://kenney.nl";
pub const KENNEY_ASSETS_HOME: &str = "https://kenney.nl/assets";
pub const KENNEY_GITHUB: &str = "https://github.com/KenneyNL";
/// Byte-identical to the pack_import / derive test fixture. The digest is
/// of this pinned test text, not a downloaded legal PDF.
pub const KENNEY_TERMS_TEXT: &[u8] = b"CC-BY-4.0 legal text for pack_import tests";
pub const KENNEY_REDISTRIBUTION: &str = "attribution-required";
pub const KENNEY_DERIVATIVES: &str = "allowed";

/// One Kenney pack the Import UI / compiler names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KenneyPack {
    pub name: &'static str,
    /// Display title as on kenney.nl (the slug is `name`).
    pub title: &'static str,
    pub version: &'static str,
    pub page: &'static str,
}

/// The Kenney catalogue this compiler knows: every free 3D kit listed on
/// kenney.nl (inventoried 2026-08-28, 4 category pages) plus the 2D/audio
/// packs in-tree tests and the sandbox use. `version` is the suffix of
/// Kenney's own zip name (`kenney_<slug>_<version>.zip`) where it has one;
/// older zips carry none and are recorded as "1.0". Kits are NOT downloaded
/// by this crate — each one is a local folder under `local/packs/kenney/<name>`
/// (flat `<stem>.glb` + optional `Textures/`), see the asset-ui Import card.
pub const KENNEY_PACKS: &[KenneyPack] = &[
    // ---- 3D kits: every free kit under kenney.nl/assets/category:3D (50 on 2026-08-28) ----
    // no GLB in the download (glTF text + Unity package only): not importable as shipped
    KenneyPack {
        name: "3d-road-tiles",
        title: "3D Road Tiles",
        version: "1.0",
        page: "https://kenney.nl/assets/3d-road-tiles",
    },
    // no GLB in the download (FBX only): not importable as shipped
    KenneyPack {
        name: "animated-characters-protagonists",
        title: "Animated Characters Protagonists",
        version: "1.0",
        page: "https://kenney.nl/assets/animated-characters-protagonists",
    },
    // no GLB in the download (FBX only): not importable as shipped
    KenneyPack {
        name: "animated-characters-retro",
        title: "Animated Characters Retro",
        version: "1.0",
        page: "https://kenney.nl/assets/animated-characters-retro",
    },
    // no GLB in the download (FBX only): not importable as shipped
    KenneyPack {
        name: "animated-characters-survivors",
        title: "Animated Characters Survivors",
        version: "1.0",
        page: "https://kenney.nl/assets/animated-characters-survivors",
    },
    KenneyPack {
        name: "blaster-kit",
        title: "Blaster Kit",
        version: "2.1",
        page: "https://kenney.nl/assets/blaster-kit",
    },
    KenneyPack {
        name: "blocky-characters",
        title: "Blocky Characters",
        version: "20",
        page: "https://kenney.nl/assets/blocky-characters",
    },
    KenneyPack {
        name: "brick-kit",
        title: "Brick Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/brick-kit",
    },
    KenneyPack {
        name: "building-kit",
        title: "Building Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/building-kit",
    },
    KenneyPack {
        name: "car-kit",
        title: "Car Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/car-kit",
    },
    KenneyPack {
        name: "castle-kit",
        title: "Castle Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/castle-kit",
    },
    KenneyPack {
        name: "city-kit-commercial",
        title: "City Kit (Commercial)",
        version: "2.1",
        page: "https://kenney.nl/assets/city-kit-commercial",
    },
    KenneyPack {
        name: "city-kit-industrial",
        title: "City Kit (Industrial)",
        version: "1.0",
        page: "https://kenney.nl/assets/city-kit-industrial",
    },
    KenneyPack {
        name: "city-kit-roads",
        title: "City Kit (Roads)",
        version: "1.0",
        page: "https://kenney.nl/assets/city-kit-roads",
    },
    KenneyPack {
        name: "city-kit-suburban",
        title: "City Kit (Suburban)",
        version: "1.0",
        page: "https://kenney.nl/assets/city-kit-suburban",
    },
    KenneyPack {
        name: "coaster-kit",
        title: "Coaster Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/coaster-kit",
    },
    KenneyPack {
        name: "cube-pets",
        title: "Cube Pets",
        version: "1.0",
        page: "https://kenney.nl/assets/cube-pets",
    },
    KenneyPack {
        name: "factory-kit",
        title: "Factory Kit",
        version: "3.0",
        page: "https://kenney.nl/assets/factory-kit",
    },
    KenneyPack {
        name: "fantasy-town-kit",
        title: "Fantasy Town Kit",
        version: "2.0",
        page: "https://kenney.nl/assets/fantasy-town-kit",
    },
    KenneyPack {
        name: "food-kit",
        title: "Food Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/food-kit",
    },
    KenneyPack {
        name: "furniture-kit",
        title: "Furniture Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/furniture-kit",
    },
    KenneyPack {
        name: "graveyard-kit",
        title: "Graveyard Kit",
        version: "5.0",
        page: "https://kenney.nl/assets/graveyard-kit",
    },
    KenneyPack {
        name: "hexagon-kit",
        title: "Hexagon Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/hexagon-kit",
    },
    KenneyPack {
        name: "holiday-kit",
        title: "Holiday Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/holiday-kit",
    },
    KenneyPack {
        name: "marble-kit",
        title: "Marble Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/marble-kit",
    },
    KenneyPack {
        name: "mini-arcade",
        title: "Mini Arcade",
        version: "1.0",
        page: "https://kenney.nl/assets/mini-arcade",
    },
    KenneyPack {
        name: "mini-arena",
        title: "Mini Arena",
        version: "1.0",
        page: "https://kenney.nl/assets/mini-arena",
    },
    KenneyPack {
        name: "mini-characters",
        title: "Mini Characters",
        version: "1.0",
        page: "https://kenney.nl/assets/mini-characters",
    },
    KenneyPack {
        name: "mini-dungeon",
        title: "Mini Dungeon",
        version: "1.0",
        page: "https://kenney.nl/assets/mini-dungeon",
    },
    KenneyPack {
        name: "mini-forest",
        title: "Mini Forest",
        version: "1.0",
        page: "https://kenney.nl/assets/mini-forest",
    },
    KenneyPack {
        name: "mini-market",
        title: "Mini Market",
        version: "1.0",
        page: "https://kenney.nl/assets/mini-market",
    },
    KenneyPack {
        name: "mini-skate",
        title: "Mini Skate",
        version: "1.0",
        page: "https://kenney.nl/assets/mini-skate",
    },
    KenneyPack {
        name: "minigolf-kit",
        title: "Minigolf Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/minigolf-kit",
    },
    KenneyPack {
        name: "modular-buildings",
        title: "Modular Buildings",
        version: "1.0",
        page: "https://kenney.nl/assets/modular-buildings",
    },
    KenneyPack {
        name: "modular-cave-kit",
        title: "Modular Cave Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/modular-cave-kit",
    },
    KenneyPack {
        name: "modular-dungeon-kit",
        title: "Modular Dungeon Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/modular-dungeon-kit",
    },
    KenneyPack {
        name: "modular-space-kit",
        title: "Modular Space Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/modular-space-kit",
    },
    KenneyPack {
        name: "nature-kit",
        title: "Nature Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/nature-kit",
    },
    KenneyPack {
        name: "pirate-kit",
        title: "Pirate Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/pirate-kit",
    },
    KenneyPack {
        name: "platformer-kit",
        title: "Platformer Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/platformer-kit",
    },
    KenneyPack {
        name: "prototype-kit",
        title: "Prototype Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/prototype-kit",
    },
    KenneyPack {
        name: "racing-kit",
        title: "Racing Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/racing-kit",
    },
    KenneyPack {
        name: "retro-fantasy-kit",
        title: "Retro Fantasy Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/retro-fantasy-kit",
    },
    KenneyPack {
        name: "retro-urban-kit",
        title: "Retro Urban Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/retro-urban-kit",
    },
    KenneyPack {
        name: "space-kit",
        title: "Space Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/space-kit",
    },
    KenneyPack {
        name: "space-station-kit",
        title: "Space Station Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/space-station-kit",
    },
    KenneyPack {
        name: "survival-kit",
        title: "Survival Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/survival-kit",
    },
    KenneyPack {
        name: "tower-defense-kit",
        title: "Tower Defense Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/tower-defense-kit",
    },
    KenneyPack {
        name: "toy-car-kit",
        title: "Toy Car Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/toy-car-kit",
    },
    KenneyPack {
        name: "train-kit",
        title: "Train Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/train-kit",
    },
    KenneyPack {
        name: "watercraft-kit",
        title: "Watercraft Kit",
        version: "1.0",
        page: "https://kenney.nl/assets/watercraft-kit",
    },
    // ---- 2D / audio packs the compiler already names ----
    KenneyPack {
        name: "ui-pack",
        title: "UI Pack",
        version: "2.0",
        page: "https://kenney.nl/assets/ui-pack",
    },
    KenneyPack {
        name: "digital-audio",
        title: "Digital Audio",
        version: "1.0",
        page: "https://kenney.nl/assets/digital-audio",
    },
    KenneyPack {
        name: "impact-sounds",
        title: "Impact Sounds",
        version: "1.0",
        page: "https://kenney.nl/assets/impact-sounds",
    },
    KenneyPack {
        name: "interface-sounds",
        title: "Interface Sounds",
        version: "1.0",
        page: "https://kenney.nl/assets/interface-sounds",
    },
    KenneyPack {
        name: "music-jingles",
        title: "Music Jingles",
        version: "1.0",
        page: "https://kenney.nl/assets/music-jingles",
    },
    KenneyPack {
        name: "rpg-audio",
        title: "RPG Audio",
        version: "1.0",
        page: "https://kenney.nl/assets/rpg-audio",
    },
    KenneyPack {
        name: "sci-fi-sounds",
        title: "Sci-Fi Sounds",
        version: "1.0",
        page: "https://kenney.nl/assets/sci-fi-sounds",
    },
    KenneyPack {
        name: "ui-audio",
        title: "UI Audio",
        version: "1.0",
        page: "https://kenney.nl/assets/ui-audio",
    },
];

pub fn kenney_terms_digest_hex() -> String {
    hex32(&sha256(KENNEY_TERMS_TEXT))
}

pub fn kenney_pack(name: &str) -> Option<&'static KenneyPack> {
    KENNEY_PACKS.iter().find(|p| p.name == name)
}

/// Kenney 3D kit slugs (the `category:3D` listing) — the packs that can
/// hold GLB models, i.e. what the model importer expects on disk.
pub fn kenney_3d_kits() -> impl Iterator<Item = &'static KenneyPack> {
    KENNEY_PACKS
        .iter()
        .filter(|p| !matches!(p.name, "ui-pack" | "digital-audio" | "impact-sounds" | "interface-sounds" | "music-jingles" | "rpg-audio" | "sci-fi-sounds" | "ui-audio"))
}

/// Official kenney.nl page of one kit (catalogued page, else the slug URL).
pub fn kenney_page(name: &str) -> String {
    kenney_pack(name)
        .map(|p| p.page.to_string())
        .unwrap_or_else(|| format!("{KENNEY_ASSETS_HOME}/{name}"))
}

/// Source/rights spec for one Kenney pack. Unknown slugs still use the same
/// explicit CC-BY-4.0 Kenney grant (never CC0); only the pack name / page
/// change. Refuse empty or illegal slugs so a caller cannot invent rights.
/// Metres per native model unit for a catalogued pack — the `real` scale
/// calibration ([`crate::dimensions`]). Kenney kits are authored at several
/// deliberate tabletop scales; each row states the measured object that
/// pins its factor, so the number can be argued with. A pack not listed
/// here is taken at its word (1 unit = 1 m) and its manifest says so.
///
/// The factor does NOT rescale the GLB: it is published as the manifest's
/// `units_per_meter` plus the `dim_*`/`scale_*` anchors, and the engine
/// applies the preset factor it picks (characters default `real`, vehicles
/// their authored `comic` play size — a Kenney car is purposely short so
/// it is fun to drive).
pub struct PackScale {
    pub pack: &'static str,
    pub metres_per_unit: f32,
    pub pin: &'static str,
}

pub const PACK_SCALES: &[PackScale] = &[
    // Authored metric — asserted, not assumed.
    PackScale { pack: "blaster-kit", metres_per_unit: 1.0, pin: "blaster-a 0.80 long = a 0.8 m carbine" },
    PackScale { pack: "building-kit", metres_per_unit: 1.0, pin: "door 2.10 m, wall 2.40 m as authored" },
    PackScale { pack: "minigolf-kit", metres_per_unit: 1.0, pin: "course wall 0.15 m as authored" },
    PackScale { pack: "modular-dungeon-kit", metres_per_unit: 1.0, pin: "gate door 4.40 m as authored" },
    PackScale { pack: "modular-cave-kit", metres_per_unit: 1.0, pin: "gate rock 4.05 m as authored" },
    PackScale { pack: "modular-space-kit", metres_per_unit: 1.0, pin: "gate door 4.62 m as authored" },
    // Humanoid-pinned: the pack's own character stands 1.75 m.
    PackScale { pack: "mini-characters", metres_per_unit: 2.5, pin: "character 0.70 tall = 1.75 m" },
    PackScale { pack: "mini-arcade", metres_per_unit: 2.5, pin: "character-gamer 0.77 tall = 1.75 m (mini family)" },
    PackScale { pack: "mini-dungeon", metres_per_unit: 2.5, pin: "character-human 0.76 tall = 1.75 m (mini family)" },
    PackScale { pack: "mini-forest", metres_per_unit: 2.5, pin: "character-archer 0.82 tall (mini family)" },
    PackScale { pack: "mini-market", metres_per_unit: 2.5, pin: "character-employee 0.72 tall (mini family)" },
    PackScale { pack: "mini-skate", metres_per_unit: 2.5, pin: "character-skate-boy 0.77 tall (mini family)" },
    PackScale { pack: "platformer-kit", metres_per_unit: 1.9, pin: "character-oobi 0.91 tall = 1.75 m" },
    PackScale { pack: "graveyard-kit", metres_per_unit: 2.0, pin: "character-keeper 0.86 tall = 1.75 m" },
    PackScale { pack: "space-kit", metres_per_unit: 2.2, pin: "astronaut 0.79 tall = 1.75 m" },
    // Furniture / props pinned by a known real object.
    PackScale { pack: "furniture-kit", metres_per_unit: 1.8, pin: "single bed 1.12 long = 2.0 m" },
    PackScale { pack: "survival-kit", metres_per_unit: 2.8, pin: "bedroll 0.61 long = 1.8 m" },
    PackScale { pack: "nature-kit", metres_per_unit: 2.5, pin: "camp bed 0.75 long = 1.9 m" },
    PackScale { pack: "coaster-kit", metres_per_unit: 1.8, pin: "park bench 0.49 seat = 0.9 m" },
    PackScale { pack: "holiday-kit", metres_per_unit: 2.1, pin: "cabin door module 1.00 = 2.1 m" },
    PackScale { pack: "space-station-kit", metres_per_unit: 1.9, pin: "single bed 1.00 long = 1.9 m" },
    PackScale { pack: "food-kit", metres_per_unit: 0.35, pin: "milk carton 0.59 tall = 0.2 m (giant-food kit)" },
    // Architecture kits: one wall module = one storey, doors walkable.
    PackScale { pack: "fantasy-town-kit", metres_per_unit: 2.0, pin: "wall module 1.00 = a 2 m storey" },
    PackScale { pack: "retro-fantasy-kit", metres_per_unit: 2.0, pin: "wall module 1.00 = a 2 m storey" },
    PackScale { pack: "retro-urban-kit", metres_per_unit: 3.0, pin: "door 0.63 tall = 1.9 m" },
    PackScale { pack: "modular-buildings", metres_per_unit: 4.8, pin: "storey block 0.62 tall = 3 m" },
    // Tabletop city / strategy kits.
    PackScale { pack: "city-kit-suburban", metres_per_unit: 5.5, pin: "two-storey house 1.14 tall = 6.3 m" },
    PackScale { pack: "city-kit-commercial", metres_per_unit: 5.5, pin: "office block 1.29 tall = 7 m" },
    PackScale { pack: "city-kit-industrial", metres_per_unit: 5.5, pin: "hall 1.47 tall = 8 m" },
    PackScale { pack: "city-kit-roads", metres_per_unit: 8.0, pin: "road tile 1.00 = an 8 m carriageway" },
    // Engine-tuned (socket.rs canonical_scale, live session): a strategy
    // board, not walk-in architecture — cottages 2.4 m at this factor.
    PackScale { pack: "hexagon-kit", metres_per_unit: 4.0, pin: "engine-tuned board scale; cottage 0.60 = 2.4 m" },
    PackScale { pack: "tower-defense-kit", metres_per_unit: 6.0, pin: "tree 0.76 tall = 4.6 m (weak pin)" },
    // Vehicles: `real` by length; their DEFAULT preset stays the authored
    // comic size (dimensions::SizeClass::Vehicle → ScalePreset::Comic).
    PackScale { pack: "car-kit", metres_per_unit: 1.7647, pin: "sedan 2.55 long = 4.5 m" },
    // Engine-tuned (socket.rs): the single-lane paved band is 0.69 of the
    // module, so x8 is 5.5 m of tarmac between kerbs. Its own racecars at
    // `real` are parade floats — drive them through the vehicle rig, or at
    // `comic`.
    PackScale { pack: "racing-kit", metres_per_unit: 8.0, pin: "engine-tuned track: paved band 0.69 module = 5.5 m tarmac" },
    PackScale { pack: "toy-car-kit", metres_per_unit: 4.8, pin: "racer 0.875 long = a drivable 4.2 m car" },
    PackScale { pack: "watercraft-kit", metres_per_unit: 1.5, pin: "rowboat 2.37 long = 3.5 m" },
    // Deliberate toys: authored units stand.
    PackScale { pack: "marble-kit", metres_per_unit: 1.0, pin: "a marble run is a toy by design" },
    PackScale { pack: "brick-kit", metres_per_unit: 1.0, pin: "toy bricks by design" },
];

pub fn pack_scale(pack_name: &str) -> Option<&'static PackScale> {
    PACK_SCALES.iter().find(|s| s.pack == pack_name)
}

pub fn kenney_spec(pack_name: &str) -> Result<PackSourceSpec, PackImportError> {
    let name = pack_name.trim();
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
        || !name.as_bytes()[0].is_ascii_lowercase()
    {
        return Err(PackImportError::new(
            PackImportErrorKind::Config,
            format!("unknown Kenney pack {pack_name}"),
        ));
    }
    let pack = kenney_pack(name);
    let version = pack.map(|p| p.version).unwrap_or("1.0");
    Ok(PackSourceSpec {
        source_id: Some(KENNEY_SOURCE_ID.into()),
        source_title: Some(KENNEY_SOURCE_TITLE.into()),
        pack_name: Some(name.into()),
        pack_version: Some(version.into()),
        license: Some(KENNEY_LICENSE.into()),
        license_revision: None,
        terms_digest: Some(kenney_terms_digest_hex()),
        terms_url: Some(KENNEY_TERMS_URL.into()),
        credits: Some(KENNEY_CREDITS.into()),
        // Collection-level origin, NOT the pack page: these rights are the
        // registered terms of the ONE `kenney` source collection, and the
        // server refuses a second registration with a different digest.
        // A per-pack URL here made every kit after the first 409. The pack
        // page is `https://kenney.nl/assets/<pack_name>` (see `kenney_page`).
        source: Some(KENNEY_ASSETS_HOME.into()),
        source_archive: None,
        redistribution: Some(KENNEY_REDISTRIBUTION.into()),
        derivatives: Some(KENNEY_DERIVATIVES.into()),
    })
}

/// Source-only identity + rights. No file list: the pack scan is the only
/// entry map. CLI values overlay a config file of the same keys.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackSourceSpec {
    pub source_id: Option<String>,
    pub source_title: Option<String>,
    pub pack_name: Option<String>,
    pub pack_version: Option<String>,
    pub license: Option<String>,
    pub license_revision: Option<String>,
    pub terms_digest: Option<String>,
    pub terms_url: Option<String>,
    pub credits: Option<String>,
    pub source: Option<String>,
    pub source_archive: Option<String>,
    pub redistribution: Option<String>,
    pub derivatives: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackImportErrorKind {
    Config,
    Rights,
    Empty,
    Unsupported,
    Special,
    Traversal,
    Collision,
    Changed,
    Malformed,
    Io,
    Content,
}

#[derive(Debug)]
pub struct PackImportError {
    pub kind: PackImportErrorKind,
    message: String,
}

impl PackImportError {
    fn new(kind: PackImportErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl PackImportErrorKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Rights => "rights",
            Self::Empty => "empty",
            Self::Unsupported => "unsupported",
            Self::Special => "special",
            Self::Traversal => "traversal",
            Self::Collision => "collision",
            Self::Changed => "changed",
            Self::Malformed => "malformed",
            Self::Io => "io",
            Self::Content => "content",
        }
    }
}

impl std::fmt::Display for PackImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind.as_str(), self.message)
    }
}

impl std::error::Error for PackImportError {}

#[cfg(test)]
thread_local! {
    static AFTER_ENUM_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
fn install_after_enum_hook(hook: impl FnOnce() + 'static) {
    AFTER_ENUM_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

fn fire_after_enum_hook() {
    #[cfg(test)]
    AFTER_ENUM_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[derive(Clone, Debug)]
pub struct PackCompileReport {
    pub source_digest: SourceCollectionId,
    pub import_revision: makepad_asset_data::ImportRevisionId,
    pub assets: usize,
    pub blobs: usize,
    pub source_path: PathBuf,
    pub manifest_path: PathBuf,
    pub plan_path: PathBuf,
    /// Models the pack ships that this importer could not represent, each
    /// with the reason. A compile SUCCEEDS with these present: one model a
    /// vendor kit happens to ship in a shape we cannot read must not cost
    /// the other three hundred. Never silent — the caller reports them.
    pub skipped_models: Vec<(String, String)>,
}

/// Progress observed while [`compile_pack_with_progress`] walks the pack:
/// files fully hashed+measured so far / discovered total, and cumulative
/// bytes hashed so far / total bytes across every discovered file (both
/// totals known upfront, from the same directory walk that already stats
/// every file). `current` names the file that just finished (its
/// pack-relative path); empty for the initial call, before the first file.
#[derive(Clone, Debug, Default)]
pub struct CompileProgress {
    pub files_done: usize,
    pub files_total: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub current: String,
}

/// Compile one local pack into canonical documents + an upload plan.
/// Does not contact the Asset Server.
pub fn compile_pack(
    pack_dir: &Path,
    out_dir: &Path,
    cli: PackSourceSpec,
    source_config: Option<&Path>,
    log: bool,
) -> Result<PackCompileReport, PackImportError> {
    compile_pack_with_progress(pack_dir, out_dir, cli, source_config, log, None)
}

/// As [`compile_pack`], with `progress` called once files are known (their
/// count and total bytes) and again after each file is fully hashed and
/// measured — this is the ONLY per-file work in a compile, so it is also
/// the only source of granular progress here. Called on every file, not
/// throttled: a big pack calls back a few hundred to a few thousand times
/// over the whole compile, which is cheap for any reasonable callback: the
/// classic-pack publish path throttles what it forwards from this into a
/// UI phase message (see `apps/asset-ui/src/import_classic.rs`).
pub fn compile_pack_with_progress(
    pack_dir: &Path,
    out_dir: &Path,
    cli: PackSourceSpec,
    source_config: Option<&Path>,
    log: bool,
    progress: Option<&mut dyn FnMut(CompileProgress)>,
) -> Result<PackCompileReport, PackImportError> {
    let mut spec = PackSourceSpec::default();
    if let Some(path) = source_config {
        spec = load_source_config(path)?;
    }
    spec.overlay(cli);
    spec.trim_in_place();
    let resolved = spec.resolve()?;

    // Derived first, as the AO bake is: vehicles get their lamp bakes
    // (`<stem>.lamps.glb`) beside the source, stale ones redone.
    let lamps = crate::vehicle_lamps::bake_tree(pack_dir);
    if log && lamps.written + lamps.removed > 0 {
        eprintln!("[asset-worker] import-pack lamps: {} written, {} fresh, {} removed", lamps.written, lamps.fresh, lamps.removed);
    }
    let pack_root = PackRoot::open(pack_dir)?;
    refuse_out_inside_pack(&pack_root.path, out_dir)?;

    let (discovered, dir_snaps) = scan_pack(&pack_root)?;
    fire_after_enum_hook();
    let built = build_manifest(&pack_root, &resolved, discovered, &dir_snaps, progress)?;
    if log {
        eprintln!(
            "[asset-worker] import-pack {} assets / {} blobs → {}",
            built.manifest.assets.len(),
            built.blobs.len(),
            out_dir.display()
        );
    }
    write_outputs(&pack_root, out_dir, &built)
}

fn refuse_out_inside_pack(pack_root: &Path, out_dir: &Path) -> Result<(), PackImportError> {
    let projected = project_out_path(out_dir)?;
    refuse_if_inside_pack(pack_root, &projected)
}

fn refuse_if_inside_pack(pack_root: &Path, projected: &Path) -> Result<(), PackImportError> {
    if projected.starts_with(pack_root) || pack_root.starts_with(projected) {
        return Err(PackImportError::new(
            PackImportErrorKind::Traversal,
            "--out must not sit inside the pack directory",
        ));
    }
    Ok(())
}

/// Lexically normalize `path` to absolute form without resolving missing
/// components. Then walk up to the first existing ancestor, canonicalize
/// that ancestor only, and project the remaining names. Never creates dirs.
fn project_out_path(out_dir: &Path) -> Result<PathBuf, PackImportError> {
    let lexical = lexical_absolute(out_dir)?;
    let (ancestor, missing) = existing_ancestor(&lexical)?;
    if missing.is_empty() {
        let meta = fs::symlink_metadata(&ancestor).map_err(|e| {
            PackImportError::new(
                PackImportErrorKind::Io,
                format!("stat --out {}: {e}", ancestor.display()),
            )
        })?;
        if meta.file_type().is_symlink() {
            return Err(PackImportError::new(
                PackImportErrorKind::Special,
                format!("--out {} is a symlink", ancestor.display()),
            ));
        }
        if meta.file_type().is_dir() {
            return canonicalize_root(&ancestor, "out");
        }
        return Err(PackImportError::new(
            PackImportErrorKind::Special,
            format!("--out {} is not a directory", ancestor.display()),
        ));
    }
    let canon = canonicalize_root(&ancestor, "out ancestor")?;
    let mut projected = canon;
    for name in missing {
        projected.push(name);
    }
    Ok(projected)
}

fn lexical_absolute(path: &Path) -> Result<PathBuf, PackImportError> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let cwd = std::env::current_dir().map_err(|e| {
            PackImportError::new(PackImportErrorKind::Io, format!("current dir: {e}"))
        })?;
        cwd.join(path)
    };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = out.pop();
            }
            Component::Normal(s) => out.push(s),
        }
    }
    Ok(out)
}

/// Deepest existing ancestor of `lexical` plus the missing names from that
/// ancestor down to `lexical` (ancestor → … → dest).
fn existing_ancestor(lexical: &Path) -> Result<(PathBuf, Vec<std::ffi::OsString>), PackImportError> {
    let mut ancestor = lexical.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(&ancestor) {
            Ok(meta) => {
                if ancestor != lexical && (meta.file_type().is_symlink() || !meta.file_type().is_dir())
                {
                    return Err(PackImportError::new(
                        PackImportErrorKind::Special,
                        format!("--out ancestor {} is not a regular directory", ancestor.display()),
                    ));
                }
                missing.reverse();
                return Ok((ancestor, missing));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let name = ancestor.file_name().map(|s| s.to_os_string()).ok_or_else(|| {
                    PackImportError::new(
                        PackImportErrorKind::Io,
                        format!("--out {} has no parent", ancestor.display()),
                    )
                })?;
                let parent = ancestor.parent().ok_or_else(|| {
                    PackImportError::new(
                        PackImportErrorKind::Io,
                        format!("--out {} has no parent", ancestor.display()),
                    )
                })?;
                if parent == ancestor {
                    return Err(PackImportError::new(
                        PackImportErrorKind::Io,
                        format!("--out {} has no existing ancestor", lexical.display()),
                    ));
                }
                missing.push(name);
                ancestor = parent.to_path_buf();
            }
            Err(e) => {
                return Err(PackImportError::new(
                    PackImportErrorKind::Io,
                    format!("stat --out {}: {e}", ancestor.display()),
                ))
            }
        }
    }
}

impl PackSourceSpec {
    fn trim_in_place(&mut self) {
        trim_slot(&mut self.source_id);
        trim_slot(&mut self.source_title);
        trim_slot(&mut self.pack_name);
        trim_slot(&mut self.pack_version);
        trim_slot(&mut self.license);
        trim_slot(&mut self.license_revision);
        trim_slot(&mut self.terms_digest);
        trim_slot(&mut self.terms_url);
        trim_slot(&mut self.credits);
        trim_slot(&mut self.source);
        trim_slot(&mut self.source_archive);
        trim_slot(&mut self.redistribution);
        trim_slot(&mut self.derivatives);
    }

    /// True when any pack-only identity field is present.
    pub fn has_pack_identity(&self) -> bool {
        self.source_id.is_some()
            || self.source_title.is_some()
            || self.pack_name.is_some()
            || self.pack_version.is_some()
            || self.terms_digest.is_some()
            || self.terms_url.is_some()
            || self.license_revision.is_some()
            || self.source_archive.is_some()
    }

    fn overlay(&mut self, other: PackSourceSpec) {
        overlay(&mut self.source_id, other.source_id);
        overlay(&mut self.source_title, other.source_title);
        overlay(&mut self.pack_name, other.pack_name);
        overlay(&mut self.pack_version, other.pack_version);
        overlay(&mut self.license, other.license);
        overlay(&mut self.license_revision, other.license_revision);
        overlay(&mut self.terms_digest, other.terms_digest);
        overlay(&mut self.terms_url, other.terms_url);
        overlay(&mut self.credits, other.credits);
        overlay(&mut self.source, other.source);
        overlay(&mut self.source_archive, other.source_archive);
        overlay(&mut self.redistribution, other.redistribution);
        overlay(&mut self.derivatives, other.derivatives);
        self.trim_in_place();
    }

    fn resolve(self) -> Result<ResolvedSource, PackImportError> {
        let require = |value: Option<String>, what: &str| -> Result<String, PackImportError> {
            match value {
                Some(s) if !s.is_empty() => Ok(s),
                _ => Err(PackImportError::new(
                    PackImportErrorKind::Rights,
                    format!("missing {what} (rights are never invented)"),
                )),
            }
        };
        let source_id = require(self.source_id, "--source-id / source_id")?;
        let source_title = require(self.source_title, "--source-title / source_title")?;
        let pack_name = require(self.pack_name, "--pack-name / pack_name")?;
        let pack_version = require(self.pack_version, "--pack-version / pack_version")?;
        let license = require(self.license, "--license / license")?;
        let terms_digest = parse_digest(
            &require(self.terms_digest, "--terms-digest / terms_digest")?,
            "terms_digest",
        )?;
        let terms_url = require(self.terms_url, "--terms-url / terms_url")?;
        let credits = require(self.credits, "--credits / credits")?;
        let source = require(self.source, "--source / source")?;
        let redistribution = parse_redistribution(&require(
            self.redistribution,
            "--redistribution / redistribution",
        )?)?;
        let derivatives =
            parse_derivatives(&require(self.derivatives, "--derivatives / derivatives")?)?;
        if (redistribution == Redistribution::AttributionRequired
            || derivatives == DerivativePolicy::AttributionRequired)
            && credits.is_empty()
        {
            return Err(PackImportError::new(
                PackImportErrorKind::Rights,
                "attribution-required rights need --credits",
            ));
        }
        if redistribution == Redistribution::Forbidden {
            return Err(PackImportError::new(
                PackImportErrorKind::Rights,
                "forbidden redistribution cannot be imported",
            ));
        }
        let license_revision = self.license_revision.unwrap_or_default();
        let source_archive = match self.source_archive {
            Some(raw) if !raw.is_empty() => Some(parse_digest(&raw, "source_archive")?),
            _ => None,
        };
        if license.len() > MAX_LICENSE_BYTES
            || license_revision.len() > MAX_LICENSE_REVISION_BYTES
            || terms_url.len() > MAX_STRING_BYTES
            || credits.len() > MAX_STRING_BYTES
            || source.len() > MAX_STRING_BYTES
            || source_title.len() > MAX_NAME_BYTES * 2
            || pack_version.len() > MAX_PACK_VERSION_BYTES
        {
            return Err(PackImportError::new(
                PackImportErrorKind::Rights,
                "a rights/identity field exceeds the content budget",
            ));
        }
        Ok(ResolvedSource {
            source_id,
            source_title,
            pack_name,
            pack_version,
            rights: Rights {
                license,
                license_revision,
                terms_digest: Some(terms_digest),
                terms_url,
                credits,
                source,
                source_archive,
                redistribution,
                derivatives,
            },
        })
    }
}

fn overlay(slot: &mut Option<String>, incoming: Option<String>) {
    if incoming.is_some() {
        *slot = incoming;
    }
}

fn trim_slot(slot: &mut Option<String>) {
    if let Some(s) = slot.as_mut() {
        let t = s.trim();
        if t.is_empty() {
            *slot = None;
        } else if t != s.as_str() {
            *s = t.to_string();
        }
    }
}

#[derive(Clone, Debug)]
struct ResolvedSource {
    source_id: String,
    source_title: String,
    pack_name: String,
    pack_version: String,
    rights: Rights,
}

const SOURCE_ONLY_KEYS: &[&str] = &[
    "source_id",
    "source_title",
    "pack_name",
    "pack_version",
    "license",
    "license_revision",
    "terms_digest",
    "terms_url",
    "credits",
    "source",
    "source_archive",
    "redistribution",
    "derivatives",
];

const FILE_LIST_KEYS: &[&str] = &["files", "assets", "entries", "paths", "items"];

fn load_source_config(path: &Path) -> Result<PackSourceSpec, PackImportError> {
    let mut file = open_path_regular_nofollow(path, "source config")?;
    let meta = file.metadata().map_err(|e| {
        PackImportError::new(
            PackImportErrorKind::Config,
            format!("source config {}: {e}", path.display()),
        )
    })?;
    if !meta.file_type().is_file() {
        return Err(PackImportError::new(
            PackImportErrorKind::Special,
            format!("source config {}: not a regular file", path.display()),
        ));
    }
    if meta.len() > MAX_SOURCE_CONFIG_BYTES {
        return Err(PackImportError::new(
            PackImportErrorKind::Config,
            format!(
                "source config exceeds {MAX_SOURCE_CONFIG_BYTES} bytes ({})",
                meta.len()
            ),
        ));
    }
    let bytes = read_exact_capped(
        &mut file,
        meta.len(),
        MAX_SOURCE_CONFIG_BYTES,
        &format!("source config {}", path.display()),
    )
    .map_err(|e| {
        if e.kind == PackImportErrorKind::Malformed || e.kind == PackImportErrorKind::Changed {
            PackImportError::new(PackImportErrorKind::Config, e.to_string())
        } else {
            e
        }
    })?;
    parse_source_config(&bytes)
}

fn open_path_regular_nofollow(path: &Path, what: &str) -> Result<File, PackImportError> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        unix::open_path_regular_nofollow(path, what)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (path, what);
        Err(pack_os_unsupported())
    }
}

fn parse_source_config(bytes: &[u8]) -> Result<PackSourceSpec, PackImportError> {
    let value = json::parse(bytes).map_err(|e| {
        PackImportError::new(PackImportErrorKind::Config, format!("source config: {e}"))
    })?;
    let obj = match &value {
        Value::Obj(pairs) => pairs,
        _ => {
            return Err(PackImportError::new(
                PackImportErrorKind::Config,
                "source config must be a JSON object",
            ))
        }
    };
    for (key, _) in obj {
        if FILE_LIST_KEYS.contains(&key.as_str()) {
            return Err(PackImportError::new(
                PackImportErrorKind::Config,
                format!("source config is source-only; refused file-list key {key}"),
            ));
        }
        if !SOURCE_ONLY_KEYS.contains(&key.as_str()) {
            return Err(PackImportError::new(
                PackImportErrorKind::Config,
                format!("unknown source config key {key}"),
            ));
        }
    }
    let text = |key: &str| -> Result<Option<String>, PackImportError> {
        match value.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Str(s)) => {
                let t = s.trim();
                if t.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(t.to_string()))
                }
            }
            Some(_) => Err(PackImportError::new(
                PackImportErrorKind::Config,
                format!("source config {key} must be a string"),
            )),
        }
    };
    Ok(PackSourceSpec {
        source_id: text("source_id")?,
        source_title: text("source_title")?,
        pack_name: text("pack_name")?,
        pack_version: text("pack_version")?,
        license: text("license")?,
        license_revision: text("license_revision")?,
        terms_digest: text("terms_digest")?,
        terms_url: text("terms_url")?,
        credits: text("credits")?,
        source: text("source")?,
        source_archive: text("source_archive")?,
        redistribution: text("redistribution")?,
        derivatives: text("derivatives")?,
    })
}

fn parse_digest(raw: &str, what: &str) -> Result<[u8; 32], PackImportError> {
    let hex = raw.strip_prefix("sha256:").unwrap_or(raw).trim();
    from_hex_exact::<32>(hex).ok_or_else(|| {
        PackImportError::new(
            PackImportErrorKind::Rights,
            format!("{what} must be 64 lowercase hex digits"),
        )
    })
}

fn parse_redistribution(text: &str) -> Result<Redistribution, PackImportError> {
    match text {
        "allowed" => Ok(Redistribution::Allowed),
        "attribution-required" => Ok(Redistribution::AttributionRequired),
        "forbidden" => Ok(Redistribution::Forbidden),
        // The classic-pack declaration: the user's own game data, served on
        // the user's LAN only.
        "user-owned-local" | "lan-local" => Ok(Redistribution::LanLocal),
        other => Err(PackImportError::new(
            PackImportErrorKind::Rights,
            format!("unknown redistribution policy {other}"),
        )),
    }
}

fn parse_derivatives(text: &str) -> Result<DerivativePolicy, PackImportError> {
    match text {
        "allowed" => Ok(DerivativePolicy::Allowed),
        "attribution-required" => Ok(DerivativePolicy::AttributionRequired),
        "forbidden" => Ok(DerivativePolicy::Forbidden),
        "local-preview-only" | "local-preview" => Ok(DerivativePolicy::LocalPreview),
        other => Err(PackImportError::new(
            PackImportErrorKind::Rights,
            format!("unknown derivatives policy {other}"),
        )),
    }
}

fn canonicalize_root(path: &Path, what: &str) -> Result<PathBuf, PackImportError> {
    let meta = fs::symlink_metadata(path).map_err(|e| {
        PackImportError::new(PackImportErrorKind::Io, format!("{what} {}: {e}", path.display()))
    })?;
    if meta.file_type().is_symlink() {
        return Err(PackImportError::new(
            PackImportErrorKind::Special,
            format!("{what} is a symlink"),
        ));
    }
    if !meta.file_type().is_dir() {
        return Err(PackImportError::new(
            PackImportErrorKind::Io,
            format!("{what} is not a directory"),
        ));
    }
    fs::canonicalize(path).map_err(|e| {
        PackImportError::new(PackImportErrorKind::Io, format!("{what} {}: {e}", path.display()))
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MediaKind {
    Png,
    Jpeg,
    Wav,
    Mp4,
    Glb,
    /// `<stem>.aomesh`: the offline AO bake's mesh (ao_uv lane) for
    /// `<stem>.glb`. A sidecar, never an asset of its own.
    AoMesh,
    /// `<stem>.ao.png`: the AO atlas the aomesh samples.
    AoPng,
    /// `<stem>.lamps.glb`: `<stem>.glb` with its lamp sockets and glowing
    /// lenses written in (`vehicle_lamps`, baked before the compile). It is
    /// published as that mesh's render GLB in place of the source one.
    LampsGlb,
    /// `<stem>.shadowsdf`: the baked shadow SDF for `<stem>.glb`.
    ShadowSdf,
    /// `<stem>.billboard`: a stateful-billboard manifest. ONE catalog asset
    /// per actor — its `Texture` file is the same-stem packed sprite sheet,
    /// its `Source` file this text, and the per-frame PNGs it names never
    /// become assets of their own.
    Billboard,
    /// `<stem>.spawn`: where a player stands in `<stem>.glb` and how high the
    /// floor, eye and step are. Published as manifest ANCHORS on the World,
    /// never as a blob — a walker must not have to fetch a second file to
    /// know it is standing on the floor.
    Spawn,
    /// `<stem>.place`: the world's actor/item placements (`world-place 1`
    /// text, self-identifying) — every THING with a billboard actor key,
    /// position and yaw. Published as a Source-role text blob beside the
    /// world GLB so a game can populate the map; a typed FileRole waits
    /// for the next schema bump (a new canon_enum tag hard-errors every
    /// deployed client).
    Place,
    /// `<stem>.grid`: the world's walkability/resource grid (`world-grid 1`
    /// text) for tiled levels — same Source-role sidecar lane as `.place`.
    Grid,
    /// Plain `ui-sheets 1` source document. Its referenced sheets remain
    /// independent Texture assets.
    UiSource,
    /// Plain `ui-font 1` source document. Its same-stem PNG is attached as
    /// the font Data asset's Texture file.
    FontSource,
}

impl MediaKind {
    fn from_ext(ext: &str) -> Option<Self> {
        match ext {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "wav" => Some(Self::Wav),
            "mp4" => Some(Self::Mp4),
            "glb" => Some(Self::Glb),
            "aomesh" => Some(Self::AoMesh),
            "shadowsdf" => Some(Self::ShadowSdf),
            "billboard" => Some(Self::Billboard),
            "spawn" => Some(Self::Spawn),
            "place" => Some(Self::Place),
            "grid" => Some(Self::Grid),
            "ui" => Some(Self::UiSource),
            "font" => Some(Self::FontSource),
            _ => None,
        }
    }

    /// Derived companion of a same-stem GLB (published as extra file roles
    /// on that mesh asset, skipped when no such GLB exists).
    fn is_sidecar(self) -> bool {
        matches!(
            self,
            Self::AoMesh | Self::AoPng | Self::LampsGlb | Self::ShadowSdf | Self::Spawn | Self::Place | Self::Grid
        )
    }

    fn media_type(self) -> MediaType {
        match self {
            Self::Png => MediaType::Png,
            Self::Jpeg => MediaType::Jpeg,
            Self::Wav => MediaType::Wav,
            Self::Mp4 => MediaType::Mp4,
            Self::Glb | Self::LampsGlb => MediaType::Glb,
            Self::AoMesh | Self::ShadowSdf => MediaType::Bin,
            Self::AoPng => MediaType::Png,
            Self::Billboard
            | Self::Spawn
            | Self::Place
            | Self::Grid
            | Self::UiSource
            | Self::FontSource => MediaType::Text,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Wav => "wav",
            Self::Mp4 => "mp4",
            Self::Glb => "glb",
            Self::AoMesh => "aomesh",
            Self::AoPng => "ao_png",
            Self::LampsGlb => "lamps_glb",
            Self::ShadowSdf => "shadowsdf",
            Self::Billboard => "billboard",
            Self::Spawn => "spawn",
            Self::Place => "place",
            Self::Grid => "grid",
            Self::UiSource => "ui",
            Self::FontSource => "font",
        }
    }
}

#[derive(Clone, Debug)]
struct DiscoveredFile {
    /// On-disk relative path (`/`-separated, original spelling).
    local_rel: String,
    /// Canonical pack path (lowercase, `/`, charset-safe, with extension).
    pack_path: String,
    /// Pack entry key (canonical path without extension).
    key: String,
    kind: MediaKind,
    snapshot: FileSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    len: u64,
    #[cfg(not(target_arch = "wasm32"))]
    modified: Option<SystemTime>,
    is_file: bool,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

impl FileIdentity {
    fn from_meta(meta: &fs::Metadata) -> Self {
        Self {
            len: meta.len(),
            #[cfg(not(target_arch = "wasm32"))]
            modified: meta.modified().ok(),
            is_file: meta.file_type().is_file() && !meta.file_type().is_symlink(),
            #[cfg(unix)]
            dev: meta.dev(),
            #[cfg(unix)]
            ino: meta.ino(),
        }
    }

    fn matches(&self, other: &Self) -> bool {
        self.len == other.len
            && {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    self.modified == other.modified
                }
                #[cfg(target_arch = "wasm32")]
                {
                    true
                }
            }
            && self.is_file
            && other.is_file
            && {
                #[cfg(unix)]
                {
                    self.dev == other.dev && self.ino == other.ino
                }
                #[cfg(not(unix))]
                {
                    true
                }
            }
    }
}

/// Scan-time snapshot kept for tests and first-look size checks.
type FileSnapshot = FileIdentity;

struct PackRoot {
    path: PathBuf,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    dir: File,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    dev: u64,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    ino: u64,
}

impl PackRoot {
    fn open(path: &Path) -> Result<Self, PackImportError> {
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = path;
            return Err(pack_os_unsupported());
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let canon = canonicalize_root(path, "pack")?;
            let dir = unix::open_dir_path(&canon)?;
            unix::bind_resolved(&dir, &canon, "pack")?;
            let meta = dir.metadata().map_err(|e| {
                PackImportError::new(PackImportErrorKind::Io, format!("fstat pack root: {e}"))
            })?;
            Ok(Self {
                path: canon,
                dir,
                dev: meta.dev(),
                ino: meta.ino(),
            })
        }
    }

    fn open_relative(&self, rel: &str, what: &str) -> Result<File, PackImportError> {
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (rel, what);
            Err(pack_os_unsupported())
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            unix::open_relative(&self.dir, &self.path, rel, what, false)
        }
    }

    fn open_relative_dir(&self, rel: &str, what: &str) -> Result<File, PackImportError> {
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (rel, what);
            Err(pack_os_unsupported())
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            unix::open_relative(&self.dir, &self.path, rel, what, true)
        }
    }
}

#[derive(Clone, Debug)]
struct DirSnapshot {
    rel: String,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    entries: Vec<(String, u8)>,
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn scan_pack(_root: &PackRoot) -> Result<(Vec<DiscoveredFile>, Vec<DirSnapshot>), PackImportError> {
    Err(pack_os_unsupported())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn scan_pack(root: &PackRoot) -> Result<(Vec<DiscoveredFile>, Vec<DirSnapshot>), PackImportError> {
    let mut files = Vec::new();
    let mut snaps = Vec::new();
    let mut dirs = 0usize;
    let mut entries = 0usize;
    walk_dir(
        &root.dir,
        "",
        root,
        0,
        &mut dirs,
        &mut entries,
        &mut files,
        &mut snaps,
    )?;
    if files.is_empty() {
        return Err(PackImportError::new(
            PackImportErrorKind::Empty,
            "pack contains no supported files",
        ));
    }
    detect_collisions(&files)?;
    files.sort_by(|a, b| a.pack_path.cmp(&b.pack_path));
    Ok((files, snaps))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug)]
enum EntryOpen<T> {
    Skip,
    SkipUnreadable(PackImportError),
    Opened(T),
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_walk_entry<T, F>(
    name: &str,
    child_rel: &str,
    d_type: u8,
    open: F,
) -> Result<EntryOpen<T>, PackImportError>
where
    F: FnOnce() -> Result<T, PackImportError>,
{
    match d_type {
        unix::DT_DIR => {
            if name.starts_with('.') {
                return Ok(EntryOpen::Skip);
            }
        }
        unix::DT_REG => {
            if skip_entry(name) {
                return Ok(EntryOpen::Skip);
            }
            if name.starts_with('.') {
                return Err(PackImportError::new(
                    PackImportErrorKind::Unsupported,
                    format!("hidden file {child_rel}"),
                ));
            }
        }
        unix::DT_LNK => {
            return Err(PackImportError::new(
                PackImportErrorKind::Special,
                format!("refusing symlink {child_rel}"),
            ));
        }
        unix::DT_UNKNOWN => {}
        _ => {
            return Err(PackImportError::new(
                PackImportErrorKind::Special,
                format!("refusing special file {child_rel}"),
            ));
        }
    }
    match open() {
        Ok(entry) => Ok(EntryOpen::Opened(entry)),
        Err(err)
            if d_type == unix::DT_UNKNOWN
                && err.kind == PackImportErrorKind::Io
                && (name.starts_with('.') || skip_entry(name)) =>
        {
            Ok(EntryOpen::SkipUnreadable(err))
        }
        Err(err) => Err(err),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn walk_dir(
    dir: &File,
    rel: &str,
    root: &PackRoot,
    depth: usize,
    dirs: &mut usize,
    entries: &mut usize,
    out: &mut Vec<DiscoveredFile>,
    snaps: &mut Vec<DirSnapshot>,
) -> Result<(), PackImportError> {
    if depth > MAX_WALK_DEPTH {
        return Err(PackImportError::new(
            PackImportErrorKind::Content,
            format!("pack directory depth exceeds {MAX_WALK_DEPTH}"),
        ));
    }
    *dirs += 1;
    if *dirs > MAX_WALK_DIRS {
        return Err(PackImportError::new(
            PackImportErrorKind::Content,
            format!("pack directory count exceeds {MAX_WALK_DIRS}"),
        ));
    }
    unix::bind_resolved(dir, &root.path, rel)?;
    let dir_entries = unix::list_dir_bounded(dir, rel, entries)?;
    let meta = dir.metadata().map_err(|e| {
        PackImportError::new(PackImportErrorKind::Io, format!("fstat dir {}: {e}", dir_label(rel)))
    })?;
    snaps.push(DirSnapshot {
        rel: rel.to_string(),
        #[cfg(unix)]
        dev: meta.dev(),
        #[cfg(unix)]
        ino: meta.ino(),
        entries: dir_entries.clone(),
    });
    for (name, d_type) in dir_entries {
        if name == "." || name == ".." {
            return Err(PackImportError::new(
                PackImportErrorKind::Traversal,
                format!("path traversal segment {name}"),
            ));
        }
        let child_rel = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let opened = match open_walk_entry(&name, &child_rel, d_type, || {
            unix::openat_entry_nofollow(dir, &name, &child_rel)
        })? {
            EntryOpen::Skip => continue,
            EntryOpen::SkipUnreadable(err) => {
                eprintln!("[asset-worker] import-pack: skipping unreadable {child_rel}: {err}");
                continue;
            }
            EntryOpen::Opened(entry) => entry,
        };
        match opened {
            unix::AtEntry::Directory(child) => {
                if name.starts_with('.') {
                    continue;
                }
                walk_dir(&child, &child_rel, root, depth + 1, dirs, entries, out, snaps)?;
            }
            unix::AtEntry::Regular(child, meta) => {
                if skip_entry(&name) {
                    continue;
                }
                if name.starts_with('.') {
                    return Err(PackImportError::new(
                        PackImportErrorKind::Unsupported,
                        format!("hidden file {child_rel}"),
                    ));
                }
                unix::bind_resolved(&child, &root.path, &child_rel)?;
                let (pack_path, key, kind) = classify_rel(&child_rel)?;
                if meta.len() == 0 {
                    return Err(PackImportError::new(
                        PackImportErrorKind::Malformed,
                        format!("{pack_path}: empty file"),
                    ));
                }
                if meta.len() > MAX_FILE_BYTES {
                    return Err(PackImportError::new(
                        PackImportErrorKind::Malformed,
                        format!("{pack_path}: exceeds MAX_FILE_BYTES"),
                    ));
                }
                out.push(DiscoveredFile {
                    local_rel: child_rel,
                    pack_path,
                    key,
                    kind,
                    snapshot: FileSnapshot::from_meta(&meta),
                });
            }
        }
    }
    Ok(())
}

// These handwritten filesystem ABIs are validated only on desktop macOS/Linux.
// Android and other Unix targets must use the fail-closed outer entry points.
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod unix {
    use super::*;
    use core::ffi::{c_char, c_int, c_uint, c_void};
    use std::ffi::{CStr, CString};
    use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd};

    const EEXIST: c_int = 17;
    const ENOTDIR: c_int = 20;
    pub(super) const DT_UNKNOWN: u8 = 0;
    pub(super) const DT_DIR: u8 = 4;
    pub(super) const DT_REG: u8 = 8;
    pub(super) const DT_LNK: u8 = 10;

    #[cfg(target_os = "macos")]
    type ModeT = u16;
    #[cfg(target_os = "macos")]
    const ELOOP: c_int = 62;
    #[cfg(target_os = "macos")]
    const EOPNOTSUPP: c_int = 102;
    #[cfg(target_os = "macos")]
    const O_NOCTTY: c_int = 0x0002_0000;
    #[cfg(target_os = "macos")]
    const O_RDONLY: c_int = 0;
    #[cfg(target_os = "macos")]
    const O_WRONLY: c_int = 0x0001;
    #[cfg(target_os = "macos")]
    const O_NONBLOCK: c_int = 0x0004;
    #[cfg(target_os = "macos")]
    pub(super) const O_DIRECTORY: c_int = 0x0010_0000;
    #[cfg(target_os = "macos")]
    const O_CLOEXEC: c_int = 0x0100_0000;
    #[cfg(target_os = "macos")]
    pub(super) const O_NOFOLLOW: c_int = 0x0100;
    #[cfg(target_os = "macos")]
    const O_CREAT: c_int = 0x0200;
    #[cfg(target_os = "macos")]
    const O_EXCL: c_int = 0x0800;
    #[cfg(target_os = "macos")]
    const F_GETPATH: c_int = 50;
    #[cfg(target_os = "macos")]
    const RENAME_EXCL: c_uint = 0x0004;

    #[cfg(target_os = "linux")]
    type ModeT = u32;
    #[cfg(target_os = "linux")]
    const ELOOP: c_int = 40;
    #[cfg(target_os = "linux")]
    const ENXIO: c_int = 6;
    #[cfg(target_os = "linux")]
    const O_NOCTTY: c_int = 0o400;
    #[cfg(target_os = "linux")]
    const O_RDONLY: c_int = 0;
    #[cfg(target_os = "linux")]
    const O_WRONLY: c_int = 0x0001;
    #[cfg(target_os = "linux")]
    const O_NONBLOCK: c_int = 0x0800;
    #[cfg(target_os = "linux")]
    const O_CLOEXEC: c_int = 0x80000;
    // Linux's O_NOFOLLOW and O_DIRECTORY differ by architecture: arm, aarch64,
    // powerpc and powerpc64 use 0o100000 and 0o40000 (where the generic values
    // mean O_LARGEFILE / O_DIRECT on arm/aarch64 and O_DIRECT / O_LARGEFILE on
    // powerpc); x86, x86_64, riscv and loongarch use the generic 0o400000 and
    // 0o200000.
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "aarch64", target_arch = "arm", target_arch = "powerpc", target_arch = "powerpc64")
    ))]
    pub(super) const O_NOFOLLOW: c_int = 0o100000;
    #[cfg(all(
        target_os = "linux",
        not(any(target_arch = "aarch64", target_arch = "arm", target_arch = "powerpc", target_arch = "powerpc64"))
    ))]
    pub(super) const O_NOFOLLOW: c_int = 0o400000;
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "aarch64", target_arch = "arm", target_arch = "powerpc", target_arch = "powerpc64")
    ))]
    pub(super) const O_DIRECTORY: c_int = 0o40000;
    #[cfg(all(
        target_os = "linux",
        not(any(target_arch = "aarch64", target_arch = "arm", target_arch = "powerpc", target_arch = "powerpc64"))
    ))]
    pub(super) const O_DIRECTORY: c_int = 0o200000;
    #[cfg(target_os = "linux")]
    const O_CREAT: c_int = 0x40;
    #[cfg(target_os = "linux")]
    const O_EXCL: c_int = 0x80;
    #[cfg(target_os = "linux")]
    const RENAME_NOREPLACE: c_uint = 1;

    pub enum AtEntry {
        Regular(File, fs::Metadata),
        Directory(File),
    }

    #[repr(C)]
    #[cfg(target_os = "macos")]
    struct Dirent {
        d_ino: u64,
        d_seekoff: u64,
        d_reclen: u16,
        d_namlen: u16,
        d_type: u8,
        d_name: [c_char; 1024],
    }

    // Matches `struct dirent` on all 64-bit Linux archs (glibc & musl) and
    // 32-bit musl, and `struct dirent64` via `readdir64` on 32-bit glibc.
    #[repr(C)]
    #[cfg(target_os = "linux")]
    struct Dirent {
        d_ino: u64,
        d_off: i64,
        d_reclen: u16,
        d_type: u8,
        d_name: [c_char; 256],
    }

    extern "C" {
        fn openat(dirfd: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
        #[cfg_attr(
            all(target_os = "macos", target_arch = "x86_64"),
            link_name = "fdopendir$INODE64"
        )]
        fn fdopendir(fd: c_int) -> *mut c_void;
        #[cfg_attr(
            all(target_os = "macos", target_arch = "x86_64"),
            link_name = "readdir$INODE64"
        )]
        #[cfg_attr(
            all(target_os = "linux", target_env = "gnu", target_pointer_width = "32"),
            link_name = "readdir64"
        )]
        fn readdir(dir: *mut c_void) -> *mut Dirent;
        fn closedir(dir: *mut c_void) -> c_int;
        fn mkdirat(dirfd: c_int, path: *const c_char, mode: ModeT) -> c_int;
        #[cfg(target_os = "macos")]
        fn renameatx_np(
            fromfd: c_int,
            from: *const c_char,
            tofd: c_int,
            to: *const c_char,
            flags: c_uint,
        ) -> c_int;
        #[cfg(target_os = "linux")]
        fn renameat2(
            olddirfd: c_int,
            oldpath: *const c_char,
            newdirfd: c_int,
            newpath: *const c_char,
            flags: c_uint,
        ) -> c_int;
        #[cfg(target_os = "macos")]
        fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
        fn getentropy(buf: *mut c_void, buflen: usize) -> c_int;
        #[cfg(target_os = "macos")]
        fn __error() -> *mut c_int;
        #[cfg(target_os = "linux")]
        fn __errno_location() -> *mut c_int;
    }

    fn is_special_open_errno(raw: Option<i32>) -> bool {
        #[cfg(target_os = "linux")]
        {
            raw == Some(ENXIO)
        }
        #[cfg(target_os = "macos")]
        {
            raw == Some(EOPNOTSUPP)
        }
    }

    fn errno_ptr() -> *mut c_int {
        #[cfg(target_os = "macos")]
        unsafe {
            __error()
        }
        #[cfg(target_os = "linux")]
        unsafe {
            __errno_location()
        }
    }

    fn set_errno(v: c_int) {
        unsafe {
            *errno_ptr() = v;
        }
    }

    fn get_errno() -> c_int {
        unsafe { *errno_ptr() }
    }

    fn component_name(name: &str, what: &str) -> Result<CString, PackImportError> {
        if name == "." || name == ".." || name.contains('/') || name.contains('\0') {
            return Err(PackImportError::new(
                PackImportErrorKind::Traversal,
                format!("{what}: illegal component name"),
            ));
        }
        CString::new(name).map_err(|_| {
            PackImportError::new(PackImportErrorKind::Malformed, format!("{what}: bad name"))
        })
    }

    pub fn open_path_regular_nofollow(path: &Path, what: &str) -> Result<File, PackImportError> {
        let meta = fs::symlink_metadata(path).map_err(|e| {
            PackImportError::new(PackImportErrorKind::Io, format!("{what}: symlink_metadata: {e}"))
        })?;
        if meta.file_type().is_symlink() {
            return Err(PackImportError::new(
                PackImportErrorKind::Special,
                format!("{what}: refusing symlink"),
            ));
        }
        if !meta.file_type().is_file() {
            return Err(PackImportError::new(
                PackImportErrorKind::Special,
                format!("{what}: not a regular file"),
            ));
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(O_NOFOLLOW | O_NONBLOCK | O_NOCTTY)
            .open(path)
            .map_err(|e| {
                let raw = e.raw_os_error();
                let kind = if raw == Some(ELOOP) || is_special_open_errno(raw) {
                    PackImportErrorKind::Special
                } else {
                    PackImportErrorKind::Io
                };
                PackImportError::new(kind, format!("{what}: open: {e}"))
            })?;
        let meta = file.metadata().map_err(|e| {
            PackImportError::new(PackImportErrorKind::Io, format!("{what}: fstat: {e}"))
        })?;
        if !meta.file_type().is_file() {
            return Err(PackImportError::new(
                PackImportErrorKind::Special,
                format!("{what}: opened handle is not a regular file"),
            ));
        }
        Ok(file)
    }

    pub fn random_hex16() -> Result<String, PackImportError> {
        let mut seed = [0u8; 16];
        let rc = unsafe { getentropy(seed.as_mut_ptr().cast::<c_void>(), seed.len()) };
        if rc != 0 {
            return Err(PackImportError::new(
                PackImportErrorKind::Io,
                format!("getentropy: {}", std::io::Error::last_os_error()),
            ));
        }
        let mut out = String::with_capacity(32);
        for b in seed {
            use std::fmt::Write;
            let _ = write!(out, "{b:02x}");
        }
        Ok(out)
    }

    pub fn open_dir_path(path: &Path) -> Result<File, PackImportError> {
        OpenOptions::new()
            .read(true)
            .custom_flags(O_DIRECTORY | O_NOFOLLOW | O_NONBLOCK | O_NOCTTY)
            .open(path)
            .map_err(|e| {
                PackImportError::new(
                    PackImportErrorKind::Io,
                    format!("open pack root {}: {e}", path.display()),
                )
            })
    }

    pub fn open_relative(
        root_dir: &File,
        root_path: &Path,
        rel: &str,
        what: &str,
        as_dir: bool,
    ) -> Result<File, PackImportError> {
        if rel.is_empty() || rel.starts_with('/') {
            return Err(PackImportError::new(
                PackImportErrorKind::Traversal,
                format!("{what}: empty or absolute relative path"),
            ));
        }
        let mut cur = root_dir.try_clone().map_err(|e| {
            PackImportError::new(PackImportErrorKind::Io, format!("{what}: {e}"))
        })?;
        let segs: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
        if segs.is_empty() {
            return Err(PackImportError::new(
                PackImportErrorKind::Traversal,
                format!("{what}: empty relative path"),
            ));
        }
        for (i, seg) in segs.iter().enumerate() {
            if *seg == "." || *seg == ".." {
                return Err(PackImportError::new(
                    PackImportErrorKind::Traversal,
                    format!("{what}: traversal segment {seg}"),
                ));
            }
            let last = i + 1 == segs.len();
            cur = openat_nofollow(&cur, seg, !last || as_dir, what)?;
        }
        bind_resolved(&cur, root_path, what)?;
        Ok(cur)
    }

    pub fn openat_entry_nofollow(
        dir: &File,
        name: &str,
        what: &str,
    ) -> Result<AtEntry, PackImportError> {
        let c = component_name(name, what)?;
        let flags = O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK | O_NOCTTY;
        let fd = unsafe { openat(dir.as_raw_fd(), c.as_ptr(), flags) };
        if fd < 0 {
            let err = std::io::Error::last_os_error();
            let raw = err.raw_os_error();
            let (kind, message) = if raw == Some(ELOOP) {
                (PackImportErrorKind::Special, format!("refusing symlink {what}"))
            } else if is_special_open_errno(raw) {
                (PackImportErrorKind::Special, format!("refusing special file {what}"))
            } else {
                (PackImportErrorKind::Io, format!("{what}: openat {name}: {err}"))
            };
            return Err(PackImportError::new(kind, message));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let meta = file.metadata().map_err(|e| {
            PackImportError::new(PackImportErrorKind::Io, format!("{what}: fstat: {e}"))
        })?;
        let ft = meta.file_type();
        if ft.is_file() {
            Ok(AtEntry::Regular(file, meta))
        } else if ft.is_dir() {
            Ok(AtEntry::Directory(file))
        } else {
            Err(PackImportError::new(
                PackImportErrorKind::Special,
                format!("refusing special file {what}"),
            ))
        }
    }

    pub fn openat_nofollow(
        dir: &File,
        name: &str,
        directory: bool,
        what: &str,
    ) -> Result<File, PackImportError> {
        let c = component_name(name, what)?;
        let mut flags = O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK | O_NOCTTY;
        if directory {
            flags |= O_DIRECTORY;
        }
        let fd = unsafe { openat(dir.as_raw_fd(), c.as_ptr(), flags) };
        if fd < 0 {
            let err = std::io::Error::last_os_error();
            let raw = err.raw_os_error();
            let (kind, message) = if raw == Some(ELOOP) {
                (PackImportErrorKind::Special, format!("{what}: refusing symlink {name}"))
            } else if directory && raw == Some(ENOTDIR) {
                (PackImportErrorKind::Special, format!("{what}: {name} is not a directory"))
            } else if is_special_open_errno(raw) {
                (PackImportErrorKind::Special, format!("{what}: refusing special file {name}"))
            } else {
                (PackImportErrorKind::Io, format!("{what}: openat {name}: {err}"))
            };
            return Err(PackImportError::new(kind, message));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if !directory {
            let meta = file.metadata().map_err(|e| {
                PackImportError::new(PackImportErrorKind::Io, format!("{what}: fstat: {e}"))
            })?;
            if !meta.file_type().is_file() {
                return Err(PackImportError::new(
                    PackImportErrorKind::Special,
                    format!("{what}: {name} is not a regular file"),
                ));
            }
        }
        Ok(file)
    }

    pub fn bind_resolved(file: &File, root: &Path, what: &str) -> Result<(), PackImportError> {
        let resolved = fd_path(file, what)?;
        let resolved = fs::canonicalize(&resolved).unwrap_or(resolved);
        if resolved != root && !resolved.starts_with(root) {
            return Err(PackImportError::new(
                PackImportErrorKind::Traversal,
                format!("{what}: opened handle resolved outside pack root"),
            ));
        }
        Ok(())
    }

    pub fn assert_fd_outside_pack(
        file: &File,
        pack: &PackRoot,
        what: &str,
    ) -> Result<PathBuf, PackImportError> {
        let resolved = fd_path(file, what)?;
        let resolved = fs::canonicalize(&resolved).unwrap_or(resolved);
        if resolved == pack.path
            || resolved.starts_with(&pack.path)
            || pack.path.starts_with(&resolved)
        {
            return Err(PackImportError::new(
                PackImportErrorKind::Traversal,
                format!("{what}: --out resolved inside the pack directory"),
            ));
        }
        let meta = file.metadata().map_err(|e| {
            PackImportError::new(PackImportErrorKind::Io, format!("{what}: fstat: {e}"))
        })?;
        if meta.dev() == pack.dev && meta.ino() == pack.ino {
            return Err(PackImportError::new(
                PackImportErrorKind::Traversal,
                format!("{what}: --out is the pack directory"),
            ));
        }
        Ok(resolved)
    }

    fn fd_path(file: &File, what: &str) -> Result<PathBuf, PackImportError> {
        #[cfg(target_os = "macos")]
        {
            let mut buf = [0 as c_char; 1024];
            let rc = unsafe { fcntl(file.as_raw_fd(), F_GETPATH, buf.as_mut_ptr()) };
            if rc != 0 {
                return Err(PackImportError::new(
                    PackImportErrorKind::Io,
                    format!("{what}: F_GETPATH: {}", std::io::Error::last_os_error()),
                ));
            }
            let cstr = unsafe { CStr::from_ptr(buf.as_ptr()) };
            let s = cstr.to_str().map_err(|_| {
                PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{what}: non-utf8 fd path"),
                )
            })?;
            Ok(PathBuf::from(s))
        }
        #[cfg(target_os = "linux")]
        {
            let link = format!("/proc/self/fd/{}", file.as_raw_fd());
            fs::read_link(&link).map_err(|e| {
                PackImportError::new(
                    PackImportErrorKind::Io,
                    format!("{what}: readlink {link}: {e}"),
                )
            })
        }
    }

    pub fn list_dir_bounded(
        dir: &File,
        rel: &str,
        entries: &mut usize,
    ) -> Result<Vec<(String, u8)>, PackImportError> {
        let before = dir.metadata().map_err(|e| {
            PackImportError::new(PackImportErrorKind::Io, format!("fstat dir {}: {e}", dir_label(rel)))
        })?;
        // Reopen from this descriptor via "." so each pass gets a new file
        // description (dup() shares the directory offset and the second
        // fdopendir/readdir would start at EOF).
        let names = list_dir_reopen(dir, rel, Some(entries))?;
        let again = list_dir_reopen(dir, rel, None)?;
        if names != again {
            return Err(PackImportError::new(
                PackImportErrorKind::Changed,
                format!("directory {} mutated during listing", dir_label(rel)),
            ));
        }
        let after = dir.metadata().map_err(|e| {
            PackImportError::new(PackImportErrorKind::Io, format!("fstat dir {}: {e}", dir_label(rel)))
        })?;
        if before.dev() != after.dev() || before.ino() != after.ino() {
            return Err(PackImportError::new(
                PackImportErrorKind::Changed,
                format!("directory {} identity changed during listing", dir_label(rel)),
            ));
        }
        Ok(names)
    }

    fn list_dir_reopen(
        dir: &File,
        rel: &str,
        entries: Option<&mut usize>,
    ) -> Result<Vec<(String, u8)>, PackImportError> {
        struct DirGuard(*mut c_void);
        impl Drop for DirGuard {
            fn drop(&mut self) {
                unsafe { closedir(self.0); }
            }
        }

        let fresh = reopen_dir(dir, rel)?;
        let dp = unsafe { fdopendir(fresh.as_raw_fd()) };
        if dp.is_null() {
            return Err(PackImportError::new(
                PackImportErrorKind::Io,
                format!("fdopendir {}: {}", dir_label(rel), std::io::Error::last_os_error()),
            ));
        }
        let _ = fresh.into_raw_fd();
        let guard = DirGuard(dp);
        collect_dir_names(guard.0, rel, entries)
    }

    fn reopen_dir(dir: &File, what: &str) -> Result<File, PackImportError> {
        let c = CString::new(".").map_err(|_| {
            PackImportError::new(PackImportErrorKind::Malformed, format!("{what}: bad name"))
        })?;
        let fd = unsafe {
            openat(
                dir.as_raw_fd(),
                c.as_ptr(),
                O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(PackImportError::new(
                PackImportErrorKind::Io,
                format!("{what}: reopen dir: {}", std::io::Error::last_os_error()),
            ));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn collect_dir_names(
        dp: *mut c_void,
        rel: &str,
        mut entries: Option<&mut usize>,
    ) -> Result<Vec<(String, u8)>, PackImportError> {
        let mut names = Vec::new();
        loop {
            set_errno(0);
            let ent = unsafe { readdir(dp) };
            if ent.is_null() {
                let err = get_errno();
                if err != 0 {
                    return Err(PackImportError::new(
                        PackImportErrorKind::Io,
                        format!("readdir {}: {}", dir_label(rel), std::io::Error::from_raw_os_error(err)),
                    ));
                }
                break;
            }
            let d_type = unsafe { core::ptr::addr_of!((*ent).d_type).read() };
            let name = unsafe { CStr::from_ptr(core::ptr::addr_of!((*ent).d_name).cast::<c_char>()) };
            let name = name.to_str().map_err(|_| {
                PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("non-utf8 name under {}", dir_label(rel)),
                )
            })?;
            if name == "." || name == ".." {
                continue;
            }
            if let Some(total) = entries.as_mut() {
                **total += 1;
                if **total > MAX_WALK_ENTRIES {
                    return Err(PackImportError::new(
                        PackImportErrorKind::Content,
                        format!("pack entry count exceeds {MAX_WALK_ENTRIES}"),
                    ));
                }
            }
            if names.len() >= MAX_DIR_ENTRIES {
                return Err(PackImportError::new(
                    PackImportErrorKind::Content,
                    format!(
                        "directory {} exceeds {MAX_DIR_ENTRIES} entries",
                        dir_label(rel)
                    ),
                ));
            }
            names.push((name.to_string(), d_type));
        }
        names.sort();
        Ok(names)
    }

    pub fn publish_bundle(
        pack: &PackRoot,
        out_dir: &Path,
        dests: &[(&str, &[u8]); 3],
    ) -> Result<(), PackImportError> {
        let lexical = lexical_absolute(out_dir)?;
        let dest_name = lexical
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                PackImportError::new(PackImportErrorKind::Malformed, "--out has no utf-8 name")
            })?
            .to_string();
        if dest_name == "." || dest_name == ".." || dest_name.contains('/') || dest_name.contains('\0')
        {
            return Err(PackImportError::new(
                PackImportErrorKind::Traversal,
                "--out name is illegal",
            ));
        }
        let (ancestor, missing) = existing_ancestor(&lexical)?;
        if missing.is_empty() {
            return Err(PackImportError::new(
                PackImportErrorKind::Io,
                format!("--out {} already exists; choose another path", out_dir.display()),
            ));
        }
        let canon = canonicalize_root(&ancestor, "out ancestor")?;
        let parent_fd = open_dir_path(&canon)?;
        bind_resolved(&parent_fd, &canon, "out ancestor")?;
        assert_fd_outside_pack(&parent_fd, pack, "out ancestor")?;
        let mut projected = canon.clone();
        for name in &missing {
            projected.push(name);
        }
        refuse_if_inside_pack(&pack.path, &projected)?;
        let mut parent = parent_fd;
        for (i, name) in missing.iter().enumerate() {
            let last = i + 1 == missing.len();
            let name = name.to_str().ok_or_else(|| {
                PackImportError::new(
                    PackImportErrorKind::Malformed,
                    "--out component is not utf-8",
                )
            })?;
            if name == "." || name == ".." || name.contains('/') || name.contains('\0') {
                return Err(PackImportError::new(
                    PackImportErrorKind::Traversal,
                    format!("--out component {name} is illegal"),
                ));
            }
            if last {
                break;
            }
            parent = mkdirat_or_open_dir(&parent, name)?;
            assert_fd_outside_pack(&parent, pack, "out parent")?;
        }
        assert_fd_outside_pack(&parent, pack, "out parent")?;
        let staging_name = format!("{STAGING_PREFIX}{}", random_hex16()?);
        mkdirat_excl(&parent, &staging_name)?;
        let staging = openat_nofollow(&parent, &staging_name, true, "staging")?;
        let published = (|| -> Result<(), PackImportError> {
            for (name, bytes) in dests {
                write_leaf_at(&staging, name, bytes)?;
            }
            staging.sync_all().map_err(|e| {
                PackImportError::new(PackImportErrorKind::Io, format!("fsync staging: {e}"))
            })?;
            exclusive_renameat(&parent, &staging_name, &dest_name)?;
            parent.sync_all().map_err(|e| {
                PackImportError::new(PackImportErrorKind::Io, format!("fsync --out parent: {e}"))
            })?;
            Ok(())
        })();
        if published.is_err() {
            let staging_path = {
                let mut p = canon;
                for name in &missing[..missing.len() - 1] {
                    p.push(name);
                }
                p.push(&staging_name);
                p
            };
            cleanup_staging(&staging_path);
        }
        published
    }

    pub(super) fn mkdirat_or_open_dir(dir: &File, name: &str) -> Result<File, PackImportError> {
        let what = format!("--out component {name}");
        let c = component_name(name, &what)?;
        let rc = unsafe { mkdirat(dir.as_raw_fd(), c.as_ptr(), 0o700) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(EEXIST) {
                return Err(PackImportError::new(
                    PackImportErrorKind::Io,
                    format!("mkdirat {name}: {err}"),
                ));
            }
        }
        openat_nofollow(dir, name, true, &what)
    }

    fn mkdirat_excl(dir: &File, name: &str) -> Result<(), PackImportError> {
        let c = CString::new(name).map_err(|_| {
            PackImportError::new(PackImportErrorKind::Malformed, format!("{name}: bad name"))
        })?;
        let rc = unsafe { mkdirat(dir.as_raw_fd(), c.as_ptr(), 0o700) };
        if rc != 0 {
            return Err(PackImportError::new(
                PackImportErrorKind::Io,
                format!("mkdirat {name}: {}", std::io::Error::last_os_error()),
            ));
        }
        Ok(())
    }

    fn write_leaf_at(dir: &File, name: &str, bytes: &[u8]) -> Result<(), PackImportError> {
        if name.contains('/') || name.contains('\0') || name == "." || name == ".." {
            return Err(PackImportError::new(
                PackImportErrorKind::Traversal,
                format!("illegal leaf {name}"),
            ));
        }
        let c = CString::new(name).map_err(|_| {
            PackImportError::new(PackImportErrorKind::Malformed, format!("{name}: bad name"))
        })?;
        let flags = O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK;
        let fd = unsafe { openat(dir.as_raw_fd(), c.as_ptr(), flags, 0o600 as c_uint) };
        if fd < 0 {
            return Err(PackImportError::new(
                PackImportErrorKind::Io,
                format!("create {name}: {}", std::io::Error::last_os_error()),
            ));
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all(bytes).map_err(|e| {
            PackImportError::new(PackImportErrorKind::Io, format!("write {name}: {e}"))
        })?;
        file.sync_all().map_err(|e| {
            PackImportError::new(PackImportErrorKind::Io, format!("fsync {name}: {e}"))
        })?;
        Ok(())
    }

    fn exclusive_renameat(dir: &File, from: &str, to: &str) -> Result<(), PackImportError> {
        let from_c = CString::new(from).map_err(|_| {
            PackImportError::new(PackImportErrorKind::Malformed, "staging name")
        })?;
        let to_c = CString::new(to).map_err(|_| {
            PackImportError::new(PackImportErrorKind::Malformed, "--out name")
        })?;
        let rc;
        #[cfg(target_os = "macos")]
        {
            rc = unsafe {
                renameatx_np(
                    dir.as_raw_fd(),
                    from_c.as_ptr(),
                    dir.as_raw_fd(),
                    to_c.as_ptr(),
                    RENAME_EXCL,
                )
            };
        }
        #[cfg(target_os = "linux")]
        {
            rc = unsafe {
                renameat2(
                    dir.as_raw_fd(),
                    from_c.as_ptr(),
                    dir.as_raw_fd(),
                    to_c.as_ptr(),
                    RENAME_NOREPLACE,
                )
            };
        }
        if rc != 0 {
            return Err(PackImportError::new(
                PackImportErrorKind::Io,
                format!("exclusive commit --out: {}", std::io::Error::last_os_error()),
            ));
        }
        Ok(())
    }

}

/// A directory's relative path as a person can read it. The pack root's
/// `rel` is the empty string, which turned every message about it into a
/// blank ("directory  exceeds 1024 entries") — the one directory every pack
/// has, and the one a reader most needs named.
fn dir_label(rel: &str) -> &str {
    if rel.is_empty() {
        "<pack root>"
    } else {
        rel
    }
}

fn skip_entry(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "license"
            | "license.txt"
            | "license.md"
            | "license.html"
            | "readme"
            | "readme.txt"
            | "readme.md"
            | "readme.html"
            | "credits.txt"
            | "credits.md"
            | "attribution.txt"
            | "desktop.ini"
            | "thumbs.db"
            | ".ds_store"
    ) || matches!(
        // `.place` is NOT here any more: it used to be a local-only
        // artifact and this list dropped it during the WALK, before
        // anything could classify it — which is why wiring `MediaKind::Place`
        // everywhere else still published no placements. `.skinao` stays
        // local.
        ext_of(&lower).as_deref(),
        Some("txt" | "md" | "html" | "htm" | "url" | "pdf" | "skinao")
    ) || lower.ends_with(".glb.shadowsdf")
}

fn ext_of(name: &str) -> Option<String> {
    let name = name.rsplit('/').next().unwrap_or(name);
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

fn classify_rel(rel: &str) -> Result<(String, String, MediaKind), PackImportError> {
    let normalized = rel.replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.contains('\0')
        || normalized.contains("//")
    {
        return Err(PackImportError::new(
            PackImportErrorKind::Traversal,
            format!("illegal pack path {rel}"),
        ));
    }
    let mut dir_parts: Vec<String> = Vec::new();
    let segments: Vec<&str> = normalized.split('/').collect();
    if segments.is_empty() {
        return Err(PackImportError::new(
            PackImportErrorKind::Traversal,
            format!("illegal pack path {rel}"),
        ));
    }
    for (i, seg) in segments.iter().enumerate() {
        if seg.is_empty() || *seg == "." || *seg == ".." {
            return Err(PackImportError::new(
                PackImportErrorKind::Traversal,
                format!("path traversal in {rel}"),
            ));
        }
        if seg.as_bytes()[0] == b'.' {
            return Err(PackImportError::new(
                PackImportErrorKind::Unsupported,
                format!("hidden segment in {rel}"),
            ));
        }
        let last = i + 1 == segments.len();
        if last {
            // `<stem>.ao.png` is the AO atlas sidecar of `<stem>.glb`: its
            // key is the GLB's, so it attaches instead of becoming a texture.
            let lower = seg.to_ascii_lowercase();
            let (ext, kind, stem) = if let Some(stem) = lower.strip_suffix(".ao.png") {
                if stem.is_empty() {
                    return Err(PackImportError::new(
                        PackImportErrorKind::Unsupported,
                        format!("unsupported file {rel}"),
                    ));
                }
                ("ao.png".to_string(), MediaKind::AoPng, seg[..stem.len()].to_string())
            } else if let Some(stem) = lower.strip_suffix(".lamps.glb").filter(|s| !s.is_empty()) {
                // `<stem>.lamps.glb` is the lamp bake of `<stem>.glb`: same key.
                ("lamps.glb".to_string(), MediaKind::LampsGlb, seg[..stem.len()].to_string())
            } else {
                let ext = ext_of(seg).ok_or_else(|| {
                    PackImportError::new(
                        PackImportErrorKind::Unsupported,
                        format!("unsupported file {rel}"),
                    )
                })?;
                let kind = MediaKind::from_ext(&ext).ok_or_else(|| {
                    PackImportError::new(
                        PackImportErrorKind::Unsupported,
                        format!("unsupported file {rel}"),
                    )
                })?;
                let stem = seg.rsplit_once('.').map(|(s, _)| s).unwrap_or(seg);
                (ext, kind, stem.to_string())
            };
            let stem = sanitize_segment(&stem, false, rel)?;
            let file_seg = format!("{stem}.{ext}");
            check_windows_reserved(&file_seg, rel)?;
            dir_parts.push(file_seg);
            let pack_path = dir_parts.join("/");
            if pack_path.len() > MAX_PACK_PATH_BYTES {
                return Err(PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{pack_path}: pack path too long"),
                ));
            }
            let key = pack_path
                .strip_suffix(&format!(".{ext}"))
                .map(str::to_string)
                .unwrap_or_else(|| pack_path.clone());
            return Ok((pack_path, key, kind));
        }
        dir_parts.push(sanitize_segment(seg, true, rel)?);
    }
    Err(PackImportError::new(
        PackImportErrorKind::Unsupported,
        format!("unsupported file {rel}"),
    ))
}

fn sanitize_segment(seg: &str, allow_dot: bool, rel: &str) -> Result<String, PackImportError> {
    let mut out = String::new();
    for c in seg.chars() {
        let c = if c.is_ascii() {
            c.to_ascii_lowercase()
        } else {
            if !out.ends_with('-') {
                out.push('-');
            }
            continue;
        };
        let ok = matches!(c, 'a'..='z' | '0'..='9' | '-' | '_') || (allow_dot && c == '.');
        if ok {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    while out.ends_with('-') || out.ends_with('.') {
        out.pop();
    }
    let out = out.trim_start_matches('-').to_string();
    if out.is_empty() {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("empty path segment after canonicalize in {rel}"),
        ));
    }
    let b0 = out.as_bytes()[0];
    if !b0.is_ascii_lowercase() && !b0.is_ascii_digit() {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("path segment must start alphanumeric in {rel}"),
        ));
    }
    check_windows_reserved(&out, rel)?;
    Ok(out)
}

fn check_windows_reserved(segment: &str, rel: &str) -> Result<(), PackImportError> {
    const RESERVED: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    let stem = segment.split('.').next().unwrap_or(segment);
    if RESERVED.contains(&stem) {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("windows-reserved path segment in {rel}"),
        ));
    }
    Ok(())
}

fn detect_collisions(files: &[DiscoveredFile]) -> Result<(), PackImportError> {
    let mut by_pack: BTreeMap<&str, &str> = BTreeMap::new();
    let mut by_fold: BTreeMap<String, &str> = BTreeMap::new();
    for f in files {
        if let Some(prev) = by_pack.insert(&f.pack_path, &f.local_rel) {
            return Err(PackImportError::new(
                PackImportErrorKind::Collision,
                format!("canonical path {} collides ({prev} vs {})", f.pack_path, f.local_rel),
            ));
        }
        let fold = f.local_rel.replace('\\', "/").to_ascii_lowercase();
        if let Some(prev) = by_fold.insert(fold, &f.local_rel) {
            return Err(PackImportError::new(
                PackImportErrorKind::Collision,
                format!("case-fold collision {prev} vs {}", f.local_rel),
            ));
        }
    }
    Ok(())
}

struct BuiltPack {
    collection: SourceCollection,
    manifest: ImportManifest,
    blobs: Vec<PlanBlob>,
    /// Models the pack ships that this importer could not represent, each
    /// with the reason it was left out. Never a silent drop: the compile
    /// report carries these up so a caller can say what did not arrive.
    skipped: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
struct PlanBlob {
    pack_path: String,
    local_rel: String,
    blob: BlobId,
    byte_len: u64,
    media: MediaKind,
    role: FileRole,
}

struct HashedFile {
    discovered: DiscoveredFile,
    blob: BlobId,
    byte_len: u64,
    dims: Option<ImageDims>,
    media_millis: u32,
    glb: Option<GlbMeasure>,
    identity: FileIdentity,
    /// False for a sidecar the renderer cannot read (stale bake format,
    /// truncated file): it is left behind, the pack still compiles.
    sidecar_ok: bool,
    /// Parsed `.billboard` manifest (states, frames, sheet layout).
    billboard: Option<StatefulBillboard>,
    /// Parsed `.spawn` sidecar (player starts + walk heights).
    nav: Option<WorldNav>,
    /// A flat / fully transparent image: never a legal thumbnail.
    placeholder: bool,
    /// The cell layout a packed sheet stamped into itself, when this image
    /// is one. Read from the bytes that were hashed, so a thumbnail declares
    /// the layout its packer WROTE instead of one a consumer measured.
    sheet: Option<(ThumbnailCells, f32)>,
}

/// The declared views of a thumbnail file: an `anim` view when the picture
/// stamped its own cell layout, nothing at all otherwise. A still that says
/// nothing about itself is honest — the alternative was every consumer
/// measuring the pixels and calling a 1024-square render a 64-frame sheet.
fn sheet_views(file: &HashedFile) -> Vec<ThumbnailView> {
    match file.sheet {
        Some((cells, fps)) => vec![ThumbnailView::cells(ThumbnailViewKind::Anim, cells, fps)],
        None => Vec::new(),
    }
}

#[derive(Debug)]
struct GlbMeasure {
    kind: AssetKind,
    triangles: u32,
    vertices: u32,
    joints: u16,
    clips: u16,
    max_texture_dim: u32,
    bounds: Bounds,
    rigged: bool,
    animated: bool,
    image_uris: Vec<String>,
    /// Source-neutral connection points declared by the GLB contract. A
    /// vendor adapter may author them, but the compiler only understands the
    /// generic names and measured transforms.
    anchors: Vec<Anchor>,
}

/// Generic vehicle contract carried by staged GLBs. Source adapters are free
/// to recognize any vendor layout, but the compiler admits a Vehicle only
/// when these four stable connection names are present exactly once.
const VEHICLE_WHEEL_CONNECTIONS: [&str; 4] = [
    "wheel_front_left",
    "wheel_front_right",
    "wheel_rear_left",
    "wheel_rear_right",
];

fn json_f32(value: &Value) -> Option<f32> {
    let n = match value {
        Value::Int(v) => *v as f64,
        Value::F64(v) => *v,
        _ => return None,
    };
    (n.is_finite() && n >= f32::MIN as f64 && n <= f32::MAX as f64).then_some(n as f32)
}

fn json_vec3_f32(value: &Value) -> Option<[f32; 3]> {
    let values = value.as_arr()?;
    if values.len() != 3 {
        return None;
    }
    Some([
        json_f32(&values[0])?,
        json_f32(&values[1])?,
        json_f32(&values[2])?,
    ])
}

/// Read the source-neutral wheel declarations an asset-side adapter authored.
/// No path, pack or vendor name participates. A partial/duplicate declaration
/// refuses instead of silently degrading a published Vehicle into a Mesh.
fn inspect_vehicle_connections(
    bytes: &[u8],
    pack_path: &str,
) -> Result<Option<Vec<Anchor>>, PackImportError> {
    let container = parse_glb_container(bytes, pack_path)?;
    let root = json::parse(container.json).map_err(|e| {
        PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: GLB JSON: {e}"),
        )
    })?;
    let Some(nodes) = root.get("nodes").and_then(Value::as_arr) else {
        return Ok(None);
    };
    let mut anchors = Vec::new();
    let mut seen = BTreeSet::new();
    for node in nodes {
        let Some(extras) = node.get("extras") else {
            continue;
        };
        if extras.get("kind").and_then(Value::as_str) != Some("vehicle_wheel") {
            continue;
        }
        let connection = extras
            .get("connection")
            .and_then(Value::as_str)
            .ok_or_else(|| glb_err(pack_path, "vehicle wheel missing connection"))?;
        if !VEHICLE_WHEEL_CONNECTIONS.contains(&connection) {
            return Err(glb_err(
                pack_path,
                format!("unknown vehicle wheel connection {connection}"),
            ));
        }
        if !seen.insert(connection.to_string()) {
            return Err(glb_err(
                pack_path,
                format!("duplicate vehicle wheel connection {connection}"),
            ));
        }
        let [x, y, z] = extras
            .get("anchor")
            .and_then(json_vec3_f32)
            .ok_or_else(|| glb_err(pack_path, format!("{connection} has invalid anchor")))?;
        let pivot_ok = extras.get("pivot").and_then(json_vec3_f32).is_some();
        let radius = extras.get("radius").and_then(json_f32).unwrap_or(0.0);
        let width = extras.get("width").and_then(json_f32).unwrap_or(0.0);
        if !pivot_ok || radius <= 0.0 || width <= 0.0 {
            return Err(glb_err(
                pack_path,
                format!("{connection} has invalid pivot/radius/width"),
            ));
        }
        anchors.push(Anchor {
            name: connection.to_string(),
            transform: Transform {
                pos: Vec3::new(x, y, z),
                rot: Quat::IDENTITY,
                scale: Vec3::ONE,
            },
        });
    }
    if anchors.is_empty() {
        return Ok(None);
    }
    if seen.len() != VEHICLE_WHEEL_CONNECTIONS.len()
        || VEHICLE_WHEEL_CONNECTIONS
            .iter()
            .any(|connection| !seen.contains(*connection))
    {
        return Err(glb_err(
            pack_path,
            format!(
                "vehicle rig has {} of {} required wheel connections",
                seen.len(),
                VEHICLE_WHEEL_CONNECTIONS.len()
            ),
        ));
    }
    anchors.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Some(anchors))
}

fn build_manifest(
    root: &PackRoot,
    source: &ResolvedSource,
    files: Vec<DiscoveredFile>,
    dir_snaps: &[DirSnapshot],
    mut progress: Option<&mut dyn FnMut(CompileProgress)>,
) -> Result<BuiltPack, PackImportError> {
    let files_total = files.len();
    let bytes_total: u64 = files.iter().map(|f| f.snapshot.len).sum();
    let mut files_done = 0usize;
    let mut bytes_done = 0u64;
    if let Some(cb) = progress.as_deref_mut() {
        cb(CompileProgress {
            files_done,
            files_total,
            bytes_done,
            bytes_total,
            current: String::new(),
        });
    }
    let mut hashed = Vec::with_capacity(files.len());
    let mut skipped: Vec<(String, String)> = Vec::new();
    let mut skipped_keys: BTreeSet<String> = BTreeSet::new();
    for file in files {
        let pack_path = file.pack_path.clone();
        let key = file.key.clone();
        let file_len = file.snapshot.len;
        match hash_and_measure(root, file) {
            Ok(h) => {
                files_done += 1;
                bytes_done += file_len;
                if let Some(cb) = progress.as_deref_mut() {
                    cb(CompileProgress {
                        files_done,
                        files_total,
                        bytes_done,
                        bytes_total,
                        current: pack_path.clone(),
                    });
                }
                hashed.push(h)
            }
            // ONE unusable entry must not cost the other three hundred.
            // A pack is a vendor's folder, not a curated set: an entry in a
            // shape this importer declares it does not SUPPORT — a model
            // binding two texture FILES, a name too long to be a catalog
            // key — is named and left out, and the rest of the kit imports.
            //
            // `Unsupported` and nothing else. `Malformed` is not a synonym
            // for it: a truncated GLB, or one pointing a texture at
            // `file:///…` (`refuse_external_uri`), means the pack is broken
            // or trying something — those still refuse the pack whole, as do
            // Io/Changed/Traversal/Special, which say the tree is moving or
            // hostile underneath us and the re-verify contract depends on
            // nothing being quietly dropped.
            Err(error) if error.kind == PackImportErrorKind::Unsupported => {
                // One line per ASSET, not per file: a model and its
                // thumbnail share a key, and both fail an over-long-key
                // check independently. Files arrive sorted by pack path, so
                // `x.glb` is recorded before `x.png` and the reason named is
                // the payload's own.
                if skipped_keys.insert(key) {
                    skipped.push((pack_path, error.to_string()));
                }
            }
            Err(error) => return Err(error),
        }
    }
    // A skipped model takes its companions with it. The thumbnail and the
    // AO/shadow sidecars beside a mesh exist to SERVE that mesh; left on
    // their own they would publish as a stray image asset — a picture of a
    // model the catalog does not have.
    if !skipped_keys.is_empty() {
        hashed.retain(|h| {
            h.discovered.kind == MediaKind::Glb || !skipped_keys.contains(&h.discovered.key)
        });
    }

    let mut thumb_for_glb: BTreeMap<String, usize> = BTreeMap::new();
    let mut used_as_thumb: BTreeSet<usize> = BTreeSet::new();
    // Baked companions of each GLB, by the GLB's key. The AO pair travels
    // together (an atlas without its mesh — or the reverse — is useless);
    // the shadow SDF stands alone. Sidecars without a GLB are dropped.
    let mut sidecars_for_glb: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    // Index of the `.spawn` sidecar per GLB key (anchors, not a blob).
    let mut nav_for_glb: BTreeMap<String, usize> = BTreeMap::new();
    // The lamp bake standing in for each GLB as its render file.
    let mut lamps_for_glb: BTreeMap<String, usize> = BTreeMap::new();
    for glb in hashed.iter().filter(|f| f.discovered.kind == MediaKind::Glb) {
        let pick = pick_thumbnail(&hashed, &glb.discovered.key);
        if let Some(idx) = pick {
            thumb_for_glb.insert(glb.discovered.key.clone(), idx);
            used_as_thumb.insert(idx);
        }
        let find = |kind: MediaKind| {
            hashed.iter().position(|f| {
                f.discovered.kind == kind && f.sidecar_ok && f.discovered.key == glb.discovered.key
            })
        };
        let mut attached = Vec::new();
        if let (Some(mesh), Some(png)) = (find(MediaKind::AoMesh), find(MediaKind::AoPng)) {
            attached.push(mesh);
            attached.push(png);
        }
        if let Some(sdf) = find(MediaKind::ShadowSdf) {
            attached.push(sdf);
        }
        // The placements sidecar rides the same lane, as a Source-role text
        // blob on the world's own asset — this is the line that makes
        // `.place` reach the catalog at all. Everything else about it was
        // already wired (extension, `is_sidecar`, media type, the
        // skip-as-its-own-asset arm, the role mapping, the parse on
        // measure), so a `.place` was read, validated, and then silently
        // dropped: 70 Doom worlds published with no placements and no
        // complaint, and a game streaming a map got an empty level.
        if let Some(place) = find(MediaKind::Place) {
            attached.push(place);
        }
        if let Some(grid) = find(MediaKind::Grid) {
            attached.push(grid);
        }
        // The spawn sidecar becomes anchors, never a file: it is metadata
        // the manifest already has room for.
        if let Some(idx) = hashed.iter().position(|f| {
            f.discovered.kind == MediaKind::Spawn
                && f.sidecar_ok
                && f.discovered.key == glb.discovered.key
        }) {
            nav_for_glb.insert(glb.discovered.key.clone(), idx);
        }
        if let Some(lamps) = find(MediaKind::LampsGlb) {
            lamps_for_glb.insert(glb.discovered.key.clone(), lamps);
        }
        if !attached.is_empty() {
            sidecars_for_glb.insert(glb.discovered.key.clone(), attached);
        }
    }

    // Images that belong to something else and must never become their own
    // catalog row: a mesh's own texture, and every file a billboard manifest
    // packed away. Deleting such a row would break its owner.
    let mut attached_images: BTreeSet<usize> = BTreeSet::new();
    let pack_has_glb = hashed.iter().any(|f| f.discovered.kind == MediaKind::Glb);
    for glb in hashed.iter().filter(|f| f.discovered.kind == MediaKind::Glb) {
        let Some(&thumb_idx) = thumb_for_glb.get(&glb.discovered.key) else {
            continue;
        };
        for uri in glb.glb.as_ref().map(|g| g.image_uris.as_slice()).unwrap_or(&[]) {
            let tex = resolve_glb_texture(&hashed, glb, uri, thumb_idx)?;
            if let Some(i) = hashed
                .iter()
                .position(|h| h.discovered.pack_path == tex.discovered.pack_path)
            {
                attached_images.insert(i);
            }
        }
    }

    // A companion `<key>_thumb` picture for an entry that is not a stateful
    // billboard: a sound's spectrogram, a single-tile sprite's padded still.
    // Without this the entry published `thumbnail: None` and drew a BLANK
    // library tile forever — a re-import could not repair it, because the
    // picture beside it was being published as its own card instead of being
    // attached (or, for a sound, ignored outright).
    let mut thumb_for_entry: BTreeMap<String, usize> = BTreeMap::new();
    for (index, file) in hashed.iter().enumerate() {
        if !matches!(file.discovered.kind, MediaKind::Png | MediaKind::Jpeg) {
            continue;
        }
        let Some(owner) = file
            .discovered
            .key
            .strip_suffix(crate::billboard_sheet::THUMB_SUFFIX)
        else {
            continue;
        };
        if file.placeholder || !thumb_dims_ok(file.dims) {
            continue;
        }
        thumb_for_entry.insert(owner.to_string(), index);
        attached_images.insert(index);
    }

    // ONE asset per stateful-billboard actor: the manifest text plus the
    // packed sheet it indexes. Its frame PNGs (483 `bossa2a8` cards, once)
    // and its preview strip stay attached, never independent.
    let mut sheet_for_billboard: BTreeMap<String, usize> = BTreeMap::new();
    let mut thumb_for_billboard: BTreeMap<String, usize> = BTreeMap::new();
    for manifest in hashed
        .iter()
        .filter(|f| f.discovered.kind == MediaKind::Billboard)
    {
        let Some(bb) = manifest.billboard.as_ref() else {
            continue;
        };
        let key = &manifest.discovered.key;
        let find_image = |want: &str| {
            hashed.iter().position(|f| {
                matches!(f.discovered.kind, MediaKind::Png | MediaKind::Jpeg)
                    && f.discovered.key == want
            })
        };
        if let Some(idx) = find_image(key) {
            sheet_for_billboard.insert(key.clone(), idx);
            attached_images.insert(idx);
        }
        if let Some(idx) = find_image(&format!("{key}{}", crate::billboard_sheet::THUMB_SUFFIX)) {
            if hashed[idx].placeholder {
                return Err(PackImportError::new(
                    PackImportErrorKind::Content,
                    format!(
                        "{}: preview strip is a placeholder (flat or transparent)",
                        hashed[idx].discovered.pack_path
                    ),
                ));
            }
            thumb_for_billboard.insert(key.clone(), idx);
            attached_images.insert(idx);
        }
        let mut seen_frames: BTreeSet<&str> = BTreeSet::new();
        for frame in &bb.frames {
            if frame.file.is_empty() || !seen_frames.insert(frame.file.as_str()) {
                continue;
            }
            let pack_path =
                resolve_relative_pack_uri(&manifest.discovered.pack_path, &frame.file)?;
            if let Some(idx) = hashed
                .iter()
                .position(|f| f.discovered.pack_path == pack_path)
            {
                attached_images.insert(idx);
            }
        }
    }

    // Bitmap-font source and its atlas are one store asset. Marking the
    // same-stem PNG attached here keeps it from also becoming a duplicate
    // Texture card when the main asset loop reaches it.
    let mut sheet_for_font: BTreeMap<String, usize> = BTreeMap::new();
    for manifest in hashed
        .iter()
        .filter(|f| f.discovered.kind == MediaKind::FontSource)
    {
        if let Some(index) = hashed.iter().position(|candidate| {
            candidate.discovered.kind == MediaKind::Png
                && candidate.discovered.key == manifest.discovered.key
        }) {
            sheet_for_font.insert(manifest.discovered.key.clone(), index);
            attached_images.insert(index);
        }
    }

    let collection = SourceCollection {
        id: source.source_id.clone(),
        title: source.source_title.clone(),
        origin: SourceOrigin::Upload,
        terms: source.rights.clone(),
    };
    let source_digest = collection.digest().map_err(|e| {
        PackImportError::new(
            PackImportErrorKind::Content,
            format!("source collection: {e}"),
        )
    })?;

    let mut assets: Vec<ImportAsset> = Vec::new();
    let mut blobs: Vec<PlanBlob> = Vec::new();
    let mut seen_blob_paths: BTreeSet<String> = BTreeSet::new();
    let mut seen_keys: BTreeSet<String> = BTreeSet::new();
    let mut seen_aliases: BTreeSet<String> = BTreeSet::new();

    for (index, file) in hashed.iter().enumerate() {
        if file.discovered.kind.is_sidecar() {
            continue;
        }
        if used_as_thumb.contains(&index) && !matches!(file.discovered.kind, MediaKind::Glb) {
            continue;
        }
        let asset = match file.discovered.kind {
            MediaKind::Png | MediaKind::Jpeg => {
                if used_as_thumb.contains(&index) || attached_images.contains(&index) {
                    continue;
                }
                // A pack that ships meshes keeps its atlases with them:
                // `Textures/colormap.png` is a shared surface, not a card.
                if pack_has_glb && is_pack_atlas(&file.discovered.pack_path) {
                    continue;
                }
                let thumb = thumb_for_entry.get(&file.discovered.key).map(|&i| &hashed[i]);
                if let Some(t) = thumb {
                    push_blob(&mut blobs, &mut seen_blob_paths, t, FileRole::PreviewFront)?;
                }
                texture_asset(file, thumb)?
            }
            MediaKind::Billboard => {
                let Some(&sheet) = sheet_for_billboard.get(&file.discovered.key) else {
                    // No packed sheet next to it: publish nothing rather
                    // than one card per frame.
                    continue;
                };
                let thumb = thumb_for_billboard
                    .get(&file.discovered.key)
                    .map(|&i| &hashed[i]);
                push_blob(&mut blobs, &mut seen_blob_paths, &hashed[sheet], FileRole::Texture)?;
                if let Some(t) = thumb.filter(|t| thumb_dims_ok(t.dims)) {
                    push_blob(&mut blobs, &mut seen_blob_paths, t, FileRole::PreviewFront)?;
                }
                billboard_asset(file, &hashed[sheet], thumb)?
            }
            MediaKind::UiSource => source_data_asset(file, None)?,
            MediaKind::FontSource => {
                let sheet = sheet_for_font
                    .get(&file.discovered.key)
                    .map(|&index| &hashed[index])
                    .ok_or_else(|| {
                        PackImportError::new(
                            PackImportErrorKind::Malformed,
                            format!(
                                "{}: bitmap font needs same-stem PNG sheet",
                                file.discovered.pack_path
                            ),
                        )
                    })?;
                push_blob(&mut blobs, &mut seen_blob_paths, sheet, FileRole::Texture)?;
                source_data_asset(file, Some(sheet))?
            }
            MediaKind::Wav => {
                let thumb = thumb_for_entry.get(&file.discovered.key).map(|&i| &hashed[i]);
                if let Some(t) = thumb {
                    push_blob(&mut blobs, &mut seen_blob_paths, t, FileRole::PreviewFront)?;
                }
                audio_asset(file, thumb)?
            }
            MediaKind::Mp4 => video_asset(file)?,
            MediaKind::Glb => {
                let thumb_idx = thumb_for_glb.get(&file.discovered.key).copied().ok_or_else(|| {
                    match rejected_thumbnail(&hashed, &file.discovered.key) {
                        Some(bad) => PackImportError::new(
                            PackImportErrorKind::Content,
                            format!(
                                "{}: thumbnail is a placeholder (flat or transparent), not a render of the asset",
                                bad.discovered.pack_path
                            ),
                        ),
                        None => PackImportError::new(
                            PackImportErrorKind::Malformed,
                            format!(
                                "{}: mesh-bearing import needs a pack PNG/JPEG thumbnail ≥ {THUMBNAIL_MIN_DIM}px",
                                file.discovered.pack_path
                            ),
                        ),
                    }
                })?;
                let albedo = match file
                    .glb
                    .as_ref()
                    .map(|g| g.image_uris.as_slice())
                    .unwrap_or(&[])
                {
                    [] => None,
                    [uri] => Some(resolve_glb_texture(&hashed, file, uri, thumb_idx)?),
                    _ => {
                        return Err(PackImportError::new(
                            PackImportErrorKind::Malformed,
                            format!(
                                "{}: unsupported multi-texture glb",
                                file.discovered.pack_path
                            ),
                        ))
                    }
                };
                if let Some(tex) = albedo {
                    push_blob(&mut blobs, &mut seen_blob_paths, tex, FileRole::Texture)?;
                }
                let sidecars: Vec<&HashedFile> = sidecars_for_glb
                    .get(&file.discovered.key)
                    .map(|idxs| idxs.iter().map(|&i| &hashed[i]).collect())
                    .unwrap_or_default();
                for sidecar in &sidecars {
                    push_blob(&mut blobs, &mut seen_blob_paths, sidecar, role_of(sidecar))?;
                }
                let nav = nav_for_glb
                    .get(&file.discovered.key)
                    .and_then(|&i| hashed[i].nav.as_ref());
                let render = lamps_for_glb.get(&file.discovered.key).map(|&i| &hashed[i]);
                mesh_asset(
                    file,
                    render,
                    &hashed[thumb_idx],
                    albedo,
                    &sidecars,
                    nav,
                    pack_scale(&source.pack_name),
                )?
            }
            MediaKind::AoMesh
            | MediaKind::AoPng
            | MediaKind::LampsGlb
            | MediaKind::ShadowSdf
            | MediaKind::Spawn
            | MediaKind::Place
            | MediaKind::Grid => continue,
        };
        if !seen_keys.insert(asset.key.as_str().to_string()) {
            return Err(PackImportError::new(
                PackImportErrorKind::Collision,
                format!("duplicate entry key {}", asset.key),
            ));
        }
        let alias = AssetAlias::new(format!(
            "{}/{}/{}",
            source.source_id, source.pack_name, asset.key
        ))
        .map_err(|e| {
            PackImportError::new(
                PackImportErrorKind::Content,
                format!("alias for {}: {e}", asset.key),
            )
        })?;
        if !seen_aliases.insert(alias.as_str().to_string()) {
            return Err(PackImportError::new(
                PackImportErrorKind::Collision,
                format!("duplicate alias {}", alias.as_str()),
            ));
        }
        // A mesh's render file is its lamp bake when it has one.
        let render = (file.discovered.kind == MediaKind::Glb)
            .then(|| lamps_for_glb.get(&file.discovered.key).map(|&i| &hashed[i]))
            .flatten()
            .unwrap_or(file);
        push_blob(&mut blobs, &mut seen_blob_paths, render, role_of(render))?;
        if file.discovered.kind == MediaKind::Glb {
            if let Some(&idx) = thumb_for_glb.get(&file.discovered.key) {
                push_blob(
                    &mut blobs,
                    &mut seen_blob_paths,
                    &hashed[idx],
                    FileRole::PreviewFront,
                )?;
            }
        }
        assets.push(asset);
    }

    if assets.is_empty() {
        return Err(PackImportError::new(
            PackImportErrorKind::Empty,
            "pack produced no importable assets",
        ));
    }
    if assets.len() > MAX_IMPORT_ASSETS {
        return Err(PackImportError::new(
            PackImportErrorKind::Content,
            format!(
                "import assets exceed {MAX_IMPORT_ASSETS} (found {})",
                assets.len()
            ),
        ));
    }

    let mut manifest = ImportManifest {
        source_collection: source_digest,
        source_id: source.source_id.clone(),
        pack_name: source.pack_name.clone(),
        pack_version: source.pack_version.clone(),
        policy_version: IMPORT_ASSET_ID_POLICY_V1,
        assets,
        rights: source.rights.clone(),
    };
    manifest.canonicalize();
    manifest.validate().map_err(|e| {
        PackImportError::new(PackImportErrorKind::Content, format!("import manifest: {e}"))
    })?;
    blobs.sort_by(|a, b| a.pack_path.cmp(&b.pack_path));
    reverify_hashed(root, &hashed)?;
    reverify_tree(root, dir_snaps)?;
    Ok(BuiltPack {
        collection,
        manifest,
        blobs,
        skipped,
    })
}

fn reverify_hashed(root: &PackRoot, hashed: &[HashedFile]) -> Result<(), PackImportError> {
    for file in hashed {
        let mut handle = root.open_relative(&file.discovered.local_rel, &file.discovered.pack_path)?;
        let now = identity_of(&handle, &file.discovered.pack_path)?;
        if !now.matches(&file.identity) {
            return Err(PackImportError::new(
                PackImportErrorKind::Changed,
                format!("{}: identity changed before plan", file.discovered.pack_path),
            ));
        }
        let (blob, len) = hash_handle(&mut handle, &file.discovered.pack_path, file.byte_len)?;
        if blob != file.blob || len != file.byte_len {
            return Err(PackImportError::new(
                PackImportErrorKind::Changed,
                format!("{}: digest changed before plan", file.discovered.pack_path),
            ));
        }
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn reverify_tree(_root: &PackRoot, _snaps: &[DirSnapshot]) -> Result<(), PackImportError> {
    Err(pack_os_unsupported())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn reverify_tree(root: &PackRoot, snaps: &[DirSnapshot]) -> Result<(), PackImportError> {
    let mut entries = 0usize;
    for snap in snaps {
        #[cfg(unix)]
        {
            let dir = if snap.rel.is_empty() {
                root.dir.try_clone().map_err(|e| {
                    PackImportError::new(PackImportErrorKind::Io, format!("clone pack root: {e}"))
                })?
            } else {
                root.open_relative_dir(&snap.rel, &snap.rel)?
            };
            unix::bind_resolved(&dir, &root.path, &snap.rel)?;
            let dir_entries = unix::list_dir_bounded(&dir, &snap.rel, &mut entries)?;
            let meta = dir.metadata().map_err(|e| {
                PackImportError::new(
                    PackImportErrorKind::Io,
                    format!("fstat dir {}: {e}", snap.rel),
                )
            })?;
            if meta.dev() != snap.dev || meta.ino() != snap.ino || dir_entries != snap.entries {
                return Err(PackImportError::new(
                    PackImportErrorKind::Changed,
                    format!(
                        "directory {} changed before plan",
                        if snap.rel.is_empty() { "." } else { snap.rel.as_str() }
                    ),
                ));
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (root, snap, &mut entries);
        }
    }
    Ok(())
}

fn role_of(file: &HashedFile) -> FileRole {
    match file.discovered.kind {
        MediaKind::Png | MediaKind::Jpeg => FileRole::Texture,
        MediaKind::Wav => FileRole::Audio,
        MediaKind::Mp4 => FileRole::Video,
        MediaKind::Glb | MediaKind::LampsGlb => FileRole::RenderGlb,
        MediaKind::AoMesh => FileRole::AoMesh,
        MediaKind::AoPng => FileRole::AoTexture,
        MediaKind::ShadowSdf => FileRole::ShadowSdf,
        MediaKind::Billboard
        | MediaKind::Spawn
        | MediaKind::Place
        | MediaKind::Grid
        | MediaKind::UiSource
        | MediaKind::FontSource => FileRole::Source,
    }
}

/// `.../textures/foo.png` — a pack-wide atlas directory at any depth.
/// Meaningless on its own, so a mesh-bearing pack keeps it attached to the
/// meshes that sample it instead of publishing an orphan card.
fn is_pack_atlas(pack_path: &str) -> bool {
    let mut segments: Vec<&str> = pack_path.split('/').collect();
    segments.pop();
    segments.iter().any(|s| s.eq_ignore_ascii_case("textures"))
}

fn push_blob(
    blobs: &mut Vec<PlanBlob>,
    seen: &mut BTreeSet<String>,
    file: &HashedFile,
    role: FileRole,
) -> Result<(), PackImportError> {
    if !seen.insert(file.discovered.pack_path.clone()) {
        return Ok(());
    }
    blobs.push(PlanBlob {
        pack_path: file.discovered.pack_path.clone(),
        local_rel: file.discovered.local_rel.clone(),
        blob: file.blob,
        byte_len: file.byte_len,
        media: file.discovered.kind,
        role,
    });
    Ok(())
}

fn thumb_dims_ok(dims: Option<ImageDims>) -> bool {
    match dims {
        Some(d) => d.width >= THUMBNAIL_MIN_DIM && d.height >= THUMBNAIL_MIN_DIM,
        None => false,
    }
}

fn pick_thumbnail(files: &[HashedFile], glb_key: &str) -> Option<usize> {
    // Same-stem / explicit association only. Pack-wide preview.png is not a
    // thumb, and neither is a placeholder tile: the catalog must never show
    // a grid of "no visual available".
    files.iter().position(|f| {
        matches!(f.discovered.kind, MediaKind::Png | MediaKind::Jpeg)
            && thumb_dims_ok(f.dims)
            && !f.placeholder
            && f.discovered.key == glb_key
    })
}

/// The same-key image a mesh WOULD have used, when it was rejected. Lets the
/// error name the file instead of saying "no thumbnail".
fn rejected_thumbnail<'a>(files: &'a [HashedFile], glb_key: &str) -> Option<&'a HashedFile> {
    files.iter().find(|f| {
        matches!(f.discovered.kind, MediaKind::Png | MediaKind::Jpeg)
            && f.discovered.key == glb_key
            && f.placeholder
    })
}

fn classic_glb_kind(pack_path: &str) -> Option<AssetKind> {
    let lower = pack_path.replace('\\', "/").to_ascii_lowercase();
    if lower.contains("/worlds/") || lower.starts_with("worlds/") {
        Some(AssetKind::World)
    } else if lower.contains("/weapons/") || lower.starts_with("weapons/") {
        Some(AssetKind::Weapon)
    } else if lower.contains("/props/") || lower.starts_with("props/") {
        Some(AssetKind::Prop)
    } else {
        None
    }
}

fn classic_image_kind(pack_path: &str) -> AssetKind {
    let lower = pack_path.replace('\\', "/").to_ascii_lowercase();
    if lower.contains("/billboards/")
        || lower.starts_with("billboards/")
        || lower.contains("/sprites/")
        || lower.starts_with("sprites/")
    {
        AssetKind::Billboard
    } else {
        AssetKind::Texture
    }
}

fn texture_asset(
    file: &HashedFile,
    thumb: Option<&HashedFile>,
) -> Result<ImportAsset, PackImportError> {
    let dims = file.dims.ok_or_else(|| {
        PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{}: missing image dims", file.discovered.pack_path),
        )
    })?;
    let byte_len = file.byte_len;
    let thumbnail = match thumb {
        Some(t) => companion_thumbnail(t)?,
        None => None,
    };
    let total_bytes = match &thumbnail {
        Some(t) => byte_len.saturating_add(t.meta.byte_len),
        None => byte_len,
    };
    Ok(ImportAsset {
        key: parse_key(&file.discovered.key)?,
        kind: classic_image_kind(&file.discovered.pack_path),
        files: vec![ImportFile {
            path: file.discovered.pack_path.clone(),
            file: AssetFile {
                role: FileRole::Texture,
                tier: DeviceTier::Any,
                lod: 0,
                media: file.discovered.kind.media_type(),
                blob: file.blob,
                byte_len,
                dims: Some(dims),
            },
        }],
        thumbnail,
        metrics: Metrics {
            total_bytes,
            triangles: 0,
            vertices: 0,
            joints: 0,
            clips: 0,
            max_texture_dim: dims.width.max(dims.height),
            media_millis: 0,
        },
        coordinate_system: PACK_COORD,
        bounds: Bounds {
            min: Vec3::ZERO,
            max: Vec3::ZERO,
        },
        anchors: Vec::new(),
        capabilities: Capabilities::default(),
        spawn_recipe: None,
    })
}

fn source_data_asset(
    source: &HashedFile,
    sheet: Option<&HashedFile>,
) -> Result<ImportAsset, PackImportError> {
    let mut files = vec![ImportFile {
        path: source.discovered.pack_path.clone(),
        file: AssetFile {
            role: FileRole::Source,
            tier: DeviceTier::Any,
            lod: 0,
            media: MediaType::Text,
            blob: source.blob,
            byte_len: source.byte_len,
            dims: None,
        },
    }];
    let mut total_bytes = source.byte_len;
    let mut max_texture_dim = 0;
    if let Some(sheet) = sheet {
        let dims = sheet.dims.ok_or_else(|| {
            PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{}: bitmap-font sheet has no dimensions", sheet.discovered.pack_path),
            )
        })?;
        total_bytes = total_bytes.checked_add(sheet.byte_len).ok_or_else(|| {
            PackImportError::new(PackImportErrorKind::Malformed, "file byte_len sum")
        })?;
        max_texture_dim = dims.width.max(dims.height);
        files.push(ImportFile {
            path: sheet.discovered.pack_path.clone(),
            file: AssetFile {
                role: FileRole::Texture,
                tier: DeviceTier::Any,
                lod: 0,
                media: sheet.discovered.kind.media_type(),
                blob: sheet.blob,
                byte_len: sheet.byte_len,
                dims: Some(dims),
            },
        });
    }
    Ok(ImportAsset {
        key: parse_key(&source.discovered.key)?,
        kind: AssetKind::Data,
        files,
        thumbnail: None,
        metrics: Metrics {
            total_bytes,
            triangles: 0,
            vertices: 0,
            joints: 0,
            clips: 0,
            max_texture_dim,
            media_millis: 0,
        },
        coordinate_system: PACK_COORD,
        bounds: Bounds {
            min: Vec3::ZERO,
            max: Vec3::ZERO,
        },
        anchors: Vec::new(),
        capabilities: Capabilities::default(),
        spawn_recipe: None,
    })
}

/// ONE `Billboard` asset per actor: the packed sprite sheet (`Texture`) plus
/// the manifest that indexes it (`Source`). The manifest's `sheet` header is
/// checked against the real sheet dimensions — a sheet that does not match
/// its cell table would cut garbage frames on the far side.
/// The `<key>_thumb` picture an entry publishes, when it has one.
///
/// The same shape [`billboard_asset`] builds for a preview strip, for the
/// entries that are not stateful billboards: a sound's spectrogram, a
/// single-tile sprite's padded still. A PNG carries its own stamped cell
/// layout, so its views are read back from the file; a JPEG carries none and
/// declares no regions — "one picture, take it as it comes", which the
/// manifest treats as no claim rather than a wrong one.
fn companion_thumbnail(t: &HashedFile) -> Result<Option<ImportThumbnail>, PackImportError> {
    if !thumb_dims_ok(t.dims) {
        return Ok(None);
    }
    let Some(d) = t.dims else { return Ok(None) };
    let media = match t.discovered.kind {
        MediaKind::Png => ThumbnailMedia::Png,
        MediaKind::Jpeg => ThumbnailMedia::Jpeg,
        _ => return Ok(None),
    };
    let views = match t.discovered.kind {
        MediaKind::Png => sheet_views(t),
        _ => Vec::new(),
    };
    Ok(Some(ImportThumbnail {
        path: t.discovered.pack_path.clone(),
        meta: ThumbnailMeta {
            blob: t.blob,
            media,
            width: d.width,
            height: d.height,
            byte_len: t.byte_len,
            views,
        },
    }))
}

fn billboard_asset(
    manifest: &HashedFile,
    sheet: &HashedFile,
    thumb: Option<&HashedFile>,
) -> Result<ImportAsset, PackImportError> {
    let dims = sheet.dims.ok_or_else(|| {
        PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{}: sprite sheet missing dims", sheet.discovered.pack_path),
        )
    })?;
    if dims.width > MAX_TEXTURE_DIM || dims.height > MAX_TEXTURE_DIM {
        return Err(PackImportError::new(
            PackImportErrorKind::Content,
            format!(
                "{}: sprite sheet exceeds MAX_TEXTURE_DIM",
                sheet.discovered.pack_path
            ),
        ));
    }
    let bb = manifest.billboard.as_ref().ok_or_else(|| {
        PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{}: unparsed billboard manifest", manifest.discovered.pack_path),
        )
    })?;
    if let Some(layout) = bb.sheet {
        let cells = bb.sheet_cells();
        let want_w = layout.cols.saturating_mul(layout.cell_w);
        let want_h = layout.rows_for(cells).saturating_mul(layout.cell_h);
        if want_w != dims.width || want_h != dims.height {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!(
                    "{}: sheet header says {want_w}x{want_h}, {} is {}x{}",
                    manifest.discovered.pack_path,
                    sheet.discovered.pack_path,
                    dims.width,
                    dims.height
                ),
            ));
        }
    }
    let mut total_bytes = sheet
        .byte_len
        .checked_add(manifest.byte_len)
        .ok_or_else(|| {
            PackImportError::new(PackImportErrorKind::Malformed, "file byte_len sum")
        })?;
    let thumbnail = match thumb.filter(|t| thumb_dims_ok(t.dims)) {
        Some(t) => {
            let d = t.dims.ok_or_else(|| {
                PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{}: thumbnail missing dims", t.discovered.pack_path),
                )
            })?;
            let media = match t.discovered.kind {
                MediaKind::Png => ThumbnailMedia::Png,
                MediaKind::Jpeg => ThumbnailMedia::Jpeg,
                _ => {
                    return Err(PackImportError::new(
                        PackImportErrorKind::Malformed,
                        format!("{}: thumbnail is not an image", t.discovered.pack_path),
                    ))
                }
            };
            total_bytes = total_bytes.checked_add(t.byte_len).ok_or_else(|| {
                PackImportError::new(PackImportErrorKind::Malformed, "file byte_len sum")
            })?;
            Some(ImportThumbnail {
                path: t.discovered.pack_path.clone(),
                meta: ThumbnailMeta {
                    blob: t.blob,
                    media,
                    width: d.width,
                    height: d.height,
                    byte_len: t.byte_len,
                    views: sheet_views(t),
                },
            })
        }
        None => None,
    };
    Ok(ImportAsset {
        key: parse_key(&manifest.discovered.key)?,
        kind: AssetKind::Billboard,
        files: vec![
            ImportFile {
                path: sheet.discovered.pack_path.clone(),
                file: AssetFile {
                    role: FileRole::Texture,
                    tier: DeviceTier::Any,
                    lod: 0,
                    media: sheet.discovered.kind.media_type(),
                    blob: sheet.blob,
                    byte_len: sheet.byte_len,
                    dims: Some(dims),
                },
            },
            ImportFile {
                path: manifest.discovered.pack_path.clone(),
                file: AssetFile {
                    role: FileRole::Source,
                    tier: DeviceTier::Any,
                    lod: 0,
                    media: MediaType::Text,
                    blob: manifest.blob,
                    byte_len: manifest.byte_len,
                    dims: None,
                },
            },
        ],
        thumbnail,
        metrics: Metrics {
            total_bytes,
            triangles: 0,
            vertices: 0,
            joints: 0,
            clips: 0,
            max_texture_dim: dims.width.max(dims.height),
            media_millis: 0,
        },
        coordinate_system: PACK_COORD,
        bounds: Bounds {
            min: Vec3::ZERO,
            max: Vec3::ZERO,
        },
        anchors: Vec::new(),
        capabilities: Capabilities::default(),
        spawn_recipe: None,
    })
}

fn audio_asset(
    file: &HashedFile,
    thumb: Option<&HashedFile>,
) -> Result<ImportAsset, PackImportError> {
    if file.media_millis == 0 {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{}: unmeasured audio duration", file.discovered.pack_path),
        ));
    }
    let thumbnail = match thumb {
        Some(t) => companion_thumbnail(t)?,
        None => None,
    };
    let total_bytes = match &thumbnail {
        Some(t) => file.byte_len.saturating_add(t.meta.byte_len),
        None => file.byte_len,
    };
    Ok(ImportAsset {
        key: parse_key(&file.discovered.key)?,
        kind: AssetKind::Audio,
        files: vec![ImportFile {
            path: file.discovered.pack_path.clone(),
            file: AssetFile {
                role: FileRole::Audio,
                tier: DeviceTier::Any,
                lod: 0,
                media: MediaType::Wav,
                blob: file.blob,
                byte_len: file.byte_len,
                dims: None,
            },
        }],
        thumbnail,
        metrics: Metrics {
            total_bytes,
            triangles: 0,
            vertices: 0,
            joints: 0,
            clips: 0,
            max_texture_dim: 0,
            media_millis: file.media_millis,
        },
        coordinate_system: PACK_COORD,
        bounds: Bounds {
            min: Vec3::ZERO,
            max: Vec3::ZERO,
        },
        anchors: Vec::new(),
        capabilities: Capabilities::default(),
        spawn_recipe: None,
    })
}

fn video_asset(file: &HashedFile) -> Result<ImportAsset, PackImportError> {
    Ok(ImportAsset {
        key: parse_key(&file.discovered.key)?,
        kind: AssetKind::Video,
        files: vec![ImportFile {
            path: file.discovered.pack_path.clone(),
            file: AssetFile {
                role: FileRole::Video,
                tier: DeviceTier::Any,
                lod: 0,
                media: MediaType::Mp4,
                blob: file.blob,
                byte_len: file.byte_len,
                dims: None,
            },
        }],
        thumbnail: None,
        metrics: Metrics {
            total_bytes: file.byte_len,
            triangles: 0,
            vertices: 0,
            joints: 0,
            clips: 0,
            max_texture_dim: 0,
            media_millis: file.media_millis,
        },
        coordinate_system: PACK_COORD,
        bounds: Bounds {
            min: Vec3::ZERO,
            max: Vec3::ZERO,
        },
        anchors: Vec::new(),
        capabilities: Capabilities::default(),
        spawn_recipe: None,
    })
}

fn resolve_glb_texture<'a>(
    hashed: &'a [HashedFile],
    glb: &HashedFile,
    uri: &str,
    thumb_idx: usize,
) -> Result<&'a HashedFile, PackImportError> {
    refuse_external_uri(uri, &glb.discovered.pack_path)?;
    let mut candidates = Vec::new();
    if let Ok(p) = resolve_relative_pack_uri(&glb.discovered.pack_path, uri) {
        candidates.push(p);
    }
    if let Ok((p, _, kind)) = classify_rel(uri.trim()) {
        if matches!(kind, MediaKind::Png | MediaKind::Jpeg) {
            candidates.push(p);
        }
    }
    candidates.sort();
    candidates.dedup();
    for pack_path in &candidates {
        if let Some(file) = hashed.iter().find(|h| h.discovered.pack_path == *pack_path) {
            if !matches!(file.discovered.kind, MediaKind::Png | MediaKind::Jpeg) {
                return Err(PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!(
                        "{}: texture uri {} is not a pack image",
                        glb.discovered.pack_path, uri
                    ),
                ));
            }
            if hashed[thumb_idx].discovered.pack_path == file.discovered.pack_path {
                return Err(PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!(
                        "{}: texture uri {} collides with the mesh thumbnail",
                        glb.discovered.pack_path, uri
                    ),
                ));
            }
            return Ok(file);
        }
    }
    Err(PackImportError::new(
        PackImportErrorKind::Malformed,
        format!(
            "{}: texture uri {} is not an atomically published pack file",
            glb.discovered.pack_path, uri
        ),
    ))
}

fn refuse_external_uri(uri: &str, pack_path: &str) -> Result<(), PackImportError> {
    let uri = uri.trim();
    if uri.is_empty() {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: empty texture uri"),
        ));
    }
    let lower = uri.to_ascii_lowercase();
    if lower.starts_with("http:")
        || lower.starts_with("https:")
        || lower.starts_with("data:")
        || lower.starts_with("file:")
        || lower.starts_with("ftp:")
        || lower.starts_with("//")
        || uri.starts_with('/')
        || uri.contains('\\')
        || uri.contains('\0')
        || uri.contains('%')
    {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: refusing external or absolute texture uri"),
        ));
    }
    if let Some((scheme, _)) = uri.split_once(':') {
        if scheme.len() == 1 || scheme.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: refusing schemed texture uri"),
            ));
        }
    }
    Ok(())
}

fn resolve_relative_pack_uri(from_file: &str, uri: &str) -> Result<String, PackImportError> {
    refuse_external_uri(uri, from_file)?;
    let mut segs: Vec<&str> = if let Some((parent, _)) = from_file.rsplit_once('/') {
        parent.split('/').filter(|s| !s.is_empty()).collect()
    } else {
        Vec::new()
    };
    for part in uri.trim().split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            if segs.pop().is_none() {
                return Err(PackImportError::new(
                    PackImportErrorKind::Traversal,
                    format!("{from_file}: texture uri escapes pack root"),
                ));
            }
            continue;
        }
        segs.push(part);
    }
    if segs.is_empty() {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{from_file}: texture uri has no path"),
        ));
    }
    let joined = segs.join("/");
    let (pack_path, _, kind) = classify_rel(&joined)?;
    if !matches!(kind, MediaKind::Png | MediaKind::Jpeg) {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{from_file}: texture uri is not an image"),
        ));
    }
    Ok(pack_path)
}

fn mesh_asset(
    glb: &HashedFile,
    render: Option<&HashedFile>,
    thumb: &HashedFile,
    albedo: Option<&HashedFile>,
    sidecars: &[&HashedFile],
    nav: Option<&WorldNav>,
    scale: Option<&PackScale>,
) -> Result<ImportAsset, PackImportError> {
    let measure = glb.glb.as_ref().ok_or_else(|| {
        PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{}: unmeasured glb", glb.discovered.pack_path),
        )
    })?;
    let dims = thumb.dims.ok_or_else(|| {
        PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{}: thumbnail missing dims", thumb.discovered.pack_path),
        )
    })?;
    let media = match thumb.discovered.kind {
        MediaKind::Png => ThumbnailMedia::Png,
        MediaKind::Jpeg => ThumbnailMedia::Jpeg,
        _ => {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{}: thumbnail is not an image", thumb.discovered.pack_path),
            ))
        }
    };
    let render = render.unwrap_or(glb);
    let mut total_bytes = render
        .byte_len
        .checked_add(thumb.byte_len)
        .ok_or_else(|| {
            PackImportError::new(PackImportErrorKind::Malformed, "file byte_len sum")
        })?;
    let mut files = vec![ImportFile {
        path: render.discovered.pack_path.clone(),
        file: AssetFile {
            role: FileRole::RenderGlb,
            tier: DeviceTier::Any,
            lod: 0,
            media: MediaType::Glb,
            blob: render.blob,
            byte_len: render.byte_len,
            dims: None,
        },
    }];
    let mut max_texture_dim = measure.max_texture_dim;
    if let Some(tex) = albedo {
        let tex_dims = tex.dims.ok_or_else(|| {
            PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{}: missing image dims", tex.discovered.pack_path),
            )
        })?;
        if tex_dims.width > MAX_TEXTURE_DIM || tex_dims.height > MAX_TEXTURE_DIM {
            return Err(PackImportError::new(
                PackImportErrorKind::Content,
                format!("{}: texture exceeds MAX_TEXTURE_DIM", tex.discovered.pack_path),
            ));
        }
        files.push(ImportFile {
            path: tex.discovered.pack_path.clone(),
            file: AssetFile {
                role: FileRole::Texture,
                tier: DeviceTier::Any,
                lod: 0,
                media: tex.discovered.kind.media_type(),
                blob: tex.blob,
                byte_len: tex.byte_len,
                dims: Some(tex_dims),
            },
        });
        total_bytes = total_bytes.checked_add(tex.byte_len).ok_or_else(|| {
            PackImportError::new(PackImportErrorKind::Malformed, "file byte_len sum")
        })?;
        max_texture_dim = max_texture_dim.max(tex_dims.width.max(tex_dims.height));
    }
    // Baked companions ride along as explicit derived roles; a game that
    // streams the mesh gets its AO and shadow bake from the same manifest.
    for sidecar in sidecars {
        let kind = sidecar.discovered.kind;
        if kind == MediaKind::AoPng && sidecar.dims.is_none() {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{}: ao atlas missing dims", sidecar.discovered.pack_path),
            ));
        }
        files.push(ImportFile {
            path: sidecar.discovered.pack_path.clone(),
            file: AssetFile {
                role: role_of(sidecar),
                tier: DeviceTier::Any,
                // `.place` and `.grid` are both Source-role text blobs on one
                // world; the manifest keys files by (role, tier, lod), so the
                // grid takes lod 1. Readers tell them apart by their magic line.
                lod: if kind == MediaKind::Grid { 1 } else { 0 },
                media: kind.media_type(),
                blob: sidecar.blob,
                byte_len: sidecar.byte_len,
                dims: sidecar.dims,
            },
        });
        total_bytes = total_bytes.checked_add(sidecar.byte_len).ok_or_else(|| {
            PackImportError::new(PackImportErrorKind::Malformed, "file byte_len sum")
        })?;
    }
    // The asset's physical size: calibrated by the pack's measured factor
    // (or taken at its authored word), stated once as `units_per_meter`
    // plus the `dim_*`/`scale_*` anchors. Worlds are baked metric by their
    // converters and carry navigation anchors instead.
    let dimensions = if measure.kind == AssetKind::World {
        None
    } else {
        let extent = [
            measure.bounds.max.x - measure.bounds.min.x,
            measure.bounds.max.y - measure.bounds.min.y,
            measure.bounds.max.z - measure.bounds.min.z,
        ];
        let (metres_per_unit, pin) = match scale {
            Some(s) => (s.metres_per_unit, s.pin),
            None => (1.0, "uncalibrated: authored units taken as metres"),
        };
        Some(crate::dimensions::Dimensions::measure(
            crate::dimensions::SizeClass::of_kind(measure.kind),
            metres_per_unit,
            extent,
            pin,
        ))
    };
    Ok(ImportAsset {
        key: parse_key(&glb.discovered.key)?,
        kind: measure.kind,
        files,
        thumbnail: Some(ImportThumbnail {
            path: thumb.discovered.pack_path.clone(),
            meta: ThumbnailMeta {
                blob: thumb.blob,
                media,
                width: dims.width,
                height: dims.height,
                byte_len: thumb.byte_len,
                views: sheet_views(thumb),
            },
        }),
        metrics: Metrics {
            total_bytes,
            triangles: measure.triangles,
            vertices: measure.vertices,
            joints: measure.joints,
            clips: measure.clips,
            max_texture_dim,
            media_millis: 0,
        },
        coordinate_system: match &dimensions {
            Some(d) => CoordinateSystem {
                units_per_meter: d.units_per_meter(),
                ..PACK_COORD
            },
            None => PACK_COORD,
        },
        bounds: measure.bounds,
        // Navigation facts of a converted map: where a player spawns, the
        // floor under them, the eye and step heights. Without these a walker
        // has to guess, and guesses walk on the ceiling.
        anchors: {
            let mut anchors = nav
                .map(WorldNav::anchors)
                .unwrap_or_else(|| measure.anchors.clone());
            if let Some(d) = &dimensions {
                anchors.extend(d.anchors());
            }
            anchors
        },
        capabilities: Capabilities {
            rigged: measure.rigged,
            animated: measure.animated,
            // A world you can spawn into is a world you can stand on: the
            // triangles ARE the collision until a collider role exists.
            collidable: measure.kind == AssetKind::World && measure.triangles > 0,
            ..Capabilities::default()
        },
        spawn_recipe: None,
    })
}

fn parse_key(key: &str) -> Result<PackEntryKey, PackImportError> {
    key.parse().map_err(|e| {
        PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("pack entry key {key}: {e}"),
        )
    })
}

fn hash_and_measure(root: &PackRoot, file: DiscoveredFile) -> Result<HashedFile, PackImportError> {
    // Can this entry's key exist at all? The key contract is the CATALOG's
    // (`MAX_KEY_SEGMENT_BYTES` and friends, in the frozen `makepad_asset_data`
    // limits), so a vendor file whose name overruns it can never publish —
    // brick-kit ships two, `square-{lq,hq}-brick-slope-corner-outside-inverted-2x2`,
    // one byte over the 48-byte segment budget.
    //
    // Asked here, before a single byte is hashed, and answered `Unsupported`
    // so it costs that entry and not the other 294. Refusing the whole pack
    // for it was the same all-or-nothing shape as the multi-texture rule:
    // an entire kit lost to one long file name.
    if let Err(error) = file.key.parse::<PackEntryKey>() {
        return Err(PackImportError::new(
            PackImportErrorKind::Unsupported,
            format!(
                "{}: entry key {} cannot exist in the catalog ({error})",
                file.pack_path, file.key
            ),
        ));
    }
    let mut handle = root.open_relative(&file.local_rel, &file.pack_path)?;
    let identity = identity_of(&handle, &file.pack_path)?;
    if !identity.is_file || !identity.matches(&file.snapshot) {
        return Err(PackImportError::new(
            PackImportErrorKind::Changed,
            format!("{}: identity changed before hashing", file.pack_path),
        ));
    }
    let (blob, byte_len) = hash_handle(&mut handle, &file.pack_path, identity.len)?;
    if byte_len != identity.len {
        return Err(PackImportError::new(
            PackImportErrorKind::Changed,
            format!("{}: size changed during hashing", file.pack_path),
        ));
    }
    handle.seek(SeekFrom::Start(0)).map_err(|e| {
        PackImportError::new(PackImportErrorKind::Io, format!("{}: {e}", file.pack_path))
    })?;
    let after_hash = identity_of(&handle, &file.pack_path)?;
    if !after_hash.matches(&identity) {
        return Err(PackImportError::new(
            PackImportErrorKind::Changed,
            format!("{}: identity changed during hashing", file.pack_path),
        ));
    }
    let measured = measure_handle(&mut handle, &file, &identity, blob)?;
    let after = identity_of(&handle, &file.pack_path)?;
    if !after.matches(&identity) {
        return Err(PackImportError::new(
            PackImportErrorKind::Changed,
            format!("{}: identity changed during measure", file.pack_path),
        ));
    }
    Ok(HashedFile {
        discovered: file,
        blob,
        byte_len,
        dims: measured.dims,
        media_millis: measured.media_millis,
        glb: measured.glb,
        identity,
        sidecar_ok: measured.sidecar_ok,
        billboard: measured.billboard,
        nav: measured.nav,
        placeholder: measured.placeholder,
        sheet: measured.sheet,
    })
}

#[cfg(test)]
fn open_regular_nofollow(path: &Path, pack_path: &str) -> Result<File, PackImportError> {
    let link = fs::symlink_metadata(path).map_err(|e| {
        PackImportError::new(PackImportErrorKind::Io, format!("{pack_path}: {e}"))
    })?;
    if link.file_type().is_symlink() || !link.file_type().is_file() {
        return Err(PackImportError::new(
            PackImportErrorKind::Special,
            format!("{pack_path}: not a regular file"),
        ));
    }
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        opts.custom_flags(unix::O_NOFOLLOW);
    }
    let file = opts.open(path).map_err(|e| {
        PackImportError::new(PackImportErrorKind::Io, format!("{pack_path}: {e}"))
    })?;
    let meta = file.metadata().map_err(|e| {
        PackImportError::new(PackImportErrorKind::Io, format!("{pack_path}: {e}"))
    })?;
    if !meta.is_file() {
        return Err(PackImportError::new(
            PackImportErrorKind::Special,
            format!("{pack_path}: opened handle is not a regular file"),
        ));
    }
    Ok(file)
}

fn identity_of(file: &File, pack_path: &str) -> Result<FileIdentity, PackImportError> {
    let meta = file.metadata().map_err(|e| {
        PackImportError::new(PackImportErrorKind::Io, format!("{pack_path}: {e}"))
    })?;
    Ok(FileIdentity::from_meta(&meta))
}

fn hash_handle(
    file: &mut File,
    pack_path: &str,
    expected: u64,
) -> Result<(BlobId, u64), PackImportError> {
    if expected == 0 {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: empty file"),
        ));
    }
    if expected > MAX_FILE_BYTES {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: exceeds MAX_FILE_BYTES"),
        ));
    }
    let mut hasher = makepad_asset_data::Sha256::new();
    let mut buf = [0u8; HASH_CHUNK];
    let mut total = 0u64;
    while total < expected {
        let want = ((expected - total) as usize).min(HASH_CHUNK);
        let n = file.read(&mut buf[..want]).map_err(|e| {
            PackImportError::new(PackImportErrorKind::Io, format!("{pack_path}: {e}"))
        })?;
        if n == 0 {
            return Err(PackImportError::new(
                PackImportErrorKind::Changed,
                format!("{pack_path}: shrunk while hashing"),
            ));
        }
        total += n as u64;
        hasher.update(&buf[..n]);
    }
    let extra = file.read(&mut buf[..1]).map_err(|e| {
        PackImportError::new(PackImportErrorKind::Io, format!("{pack_path}: {e}"))
    })?;
    if extra != 0 {
        return Err(PackImportError::new(
            PackImportErrorKind::Changed,
            format!("{pack_path}: grew while hashing"),
        ));
    }
    Ok((BlobId::from_bytes(hasher.finalize()), total))
}

#[cfg(test)]
fn hash_regular_file(
    path: &Path,
    expected: &FileSnapshot,
    pack_path: &str,
) -> Result<(BlobId, u64), PackImportError> {
    let mut handle = open_regular_nofollow(path, pack_path)?;
    let now = identity_of(&handle, pack_path)?;
    if !now.matches(expected) {
        return Err(PackImportError::new(
            PackImportErrorKind::Changed,
            format!("{pack_path}: changed before hashing"),
        ));
    }
    let (blob, total) = hash_handle(&mut handle, pack_path, expected.len)?;
    if total != expected.len {
        return Err(PackImportError::new(
            PackImportErrorKind::Changed,
            format!("{pack_path}: size changed during hashing"),
        ));
    }
    let after = identity_of(&handle, pack_path)?;
    if !after.matches(expected) {
        return Err(PackImportError::new(
            PackImportErrorKind::Changed,
            format!("{pack_path}: changed while hashing"),
        ));
    }
    Ok((blob, total))
}

#[cfg(test)]
fn recheck_unchanged(
    path: &Path,
    expected: &FileSnapshot,
    pack_path: &str,
) -> Result<(), PackImportError> {
    let meta = fs::symlink_metadata(path).map_err(|e| {
        PackImportError::new(PackImportErrorKind::Io, format!("{pack_path}: {e}"))
    })?;
    let now = FileSnapshot::from_meta(&meta);
    if !now.matches(expected) {
        return Err(PackImportError::new(
            PackImportErrorKind::Changed,
            format!("{pack_path}: changed while compiling"),
        ));
    }
    Ok(())
}

/// `(image dims, media millis, glb measure, sidecar readable)`.
/// What one file's bytes say about itself (dimensions, duration, mesh
/// shape, manifest contents). Everything here is measured from the bytes
/// that were just hashed, never from the file name.
#[derive(Default)]
struct Measured {
    dims: Option<ImageDims>,
    media_millis: u32,
    glb: Option<GlbMeasure>,
    /// False for a derived sidecar this build cannot read (it is left
    /// behind instead of refusing the whole pack).
    sidecar_ok: bool,
    billboard: Option<StatefulBillboard>,
    nav: Option<WorldNav>,
    /// True for an image that is a flat/placeholder tile, never a picture of
    /// the asset. Measured from the bytes that were just hashed.
    placeholder: bool,
    /// The stamped cell layout of a packed sheet, read from those same bytes.
    sheet: Option<(ThumbnailCells, f32)>,
}

impl Measured {
    fn ok() -> Self {
        Self { sidecar_ok: true, ..Self::default() }
    }

    fn image(w: u32, h: u32) -> Self {
        Self { dims: Some(ImageDims { width: w, height: h }), ..Self::ok() }
    }

    fn thumbnailable_image(w: u32, h: u32, bytes: &[u8]) -> Self {
        Self {
            placeholder: thumbnail_is_placeholder(bytes),
            sheet: crate::anim_icon::read_layout(bytes),
            ..Self::image(w, h)
        }
    }
}

fn measure_handle(
    handle: &mut File,
    file: &DiscoveredFile,
    identity: &FileIdentity,
    hashed: BlobId,
) -> Result<Measured, PackImportError> {
    match file.kind {
        MediaKind::Png => {
            let bytes = read_all_from(handle, identity.len, &file.pack_path)?;
            let (w, h) = validate_png(&bytes, &file.pack_path)?;
            Ok(Measured::thumbnailable_image(w, h, &bytes))
        }
        MediaKind::Jpeg => {
            let bytes = read_all_from(handle, identity.len, &file.pack_path)?;
            let (w, h) = validate_jpeg(&bytes, &file.pack_path)?;
            Ok(Measured::thumbnailable_image(w, h, &bytes))
        }
        MediaKind::Wav => {
            let bytes = read_all_from(handle, identity.len, &file.pack_path)?;
            let millis = validate_wav(&bytes, &file.pack_path)?;
            Ok(Measured { media_millis: millis, ..Measured::ok() })
        }
        MediaKind::Mp4 => {
            let bytes = read_all_from(handle, identity.len, &file.pack_path)?;
            validate_mp4_structure(&bytes, &file.pack_path)?;
            let now = identity_of(handle, &file.pack_path)?;
            if !now.matches(identity) {
                return Err(PackImportError::new(
                    PackImportErrorKind::Changed,
                    format!("{}: identity changed before video probe", file.pack_path),
                ));
            }
            if BlobId::hash_of(&bytes) != hashed {
                return Err(PackImportError::new(
                    PackImportErrorKind::Changed,
                    format!("{}: digest drifted before video probe", file.pack_path),
                ));
            }
            let millis = probe_mp4_trusted(&bytes, hashed, &file.pack_path)?;
            let after = identity_of(handle, &file.pack_path)?;
            if !after.matches(identity) {
                return Err(PackImportError::new(
                    PackImportErrorKind::Changed,
                    format!("{}: identity changed during video probe", file.pack_path),
                ));
            }
            Ok(Measured { media_millis: millis, ..Measured::ok() })
        }
        MediaKind::Glb => {
            let bytes = read_all_from(handle, identity.len, &file.pack_path)?;
            Ok(Measured { glb: Some(measure_glb(&bytes, &file.pack_path)?), ..Measured::ok() })
        }
        // Sidecars are derived caches, not source: one the renderer cannot
        // read (older bake format, truncated) is left behind rather than
        // refusing the whole pack.
        MediaKind::AoPng => {
            let bytes = read_all_from(handle, identity.len, &file.pack_path)?;
            Ok(match validate_png(&bytes, &file.pack_path) {
                Ok((w, h)) => Measured::image(w, h),
                Err(_) => Measured::default(),
            })
        }
        MediaKind::AoMesh => {
            let bytes = read_all_from(handle, identity.len, &file.pack_path)?;
            Ok(Measured {
                sidecar_ok: StaticModel::from_aomesh(&bytes).is_some(),
                ..Measured::default()
            })
        }
        MediaKind::ShadowSdf => Ok(Measured::ok()),
        // A lamp bake the importer cannot read is left behind; the source
        // GLB is published instead.
        MediaKind::LampsGlb => {
            let bytes = read_all_from(handle, identity.len, &file.pack_path)?;
            Ok(Measured { sidecar_ok: measure_glb(&bytes, &file.pack_path).is_ok(), ..Measured::default() })
        }
        // A spawn sidecar this build cannot read is left behind (the world
        // still publishes) — it is a convenience, not the payload.
        MediaKind::Spawn => {
            let bytes = read_all_from(handle, identity.len, &file.pack_path)?;
            let nav = std::str::from_utf8(&bytes).ok().and_then(WorldNav::parse);
            Ok(Measured {
                sidecar_ok: nav.is_some(),
                nav,
                ..Measured::default()
            })
        }
        // Same convenience contract as Spawn: an unreadable placement
        // sidecar is left behind, the world still publishes.
        MediaKind::Place => {
            let bytes = read_all_from(handle, identity.len, &file.pack_path)?;
            let ok = std::str::from_utf8(&bytes)
                .ok()
                .and_then(|text| crate::world_place::WorldPlace::parse(text).ok())
                .is_some();
            Ok(Measured { sidecar_ok: ok, ..Measured::default() })
        }
        MediaKind::Grid => {
            let bytes = read_all_from(handle, identity.len, &file.pack_path)?;
            let ok = std::str::from_utf8(&bytes)
                .ok()
                .and_then(|text| makepad_asset_data::world_grid::WorldGrid::parse(text).ok())
                .is_some();
            Ok(Measured { sidecar_ok: ok, ..Measured::default() })
        }
        MediaKind::UiSource | MediaKind::FontSource => {
            let bytes = read_all_from(handle, identity.len, &file.pack_path)?;
            let text = std::str::from_utf8(&bytes).map_err(|_| {
                PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{}: source manifest is not utf-8", file.pack_path),
                )
            })?;
            let magic = if file.kind == MediaKind::UiSource {
                "ui-sheets 1"
            } else {
                "ui-font 1"
            };
            if text.lines().next() != Some(magic) {
                return Err(PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{}: expected {magic} header", file.pack_path),
                ));
            }
            Ok(Measured::ok())
        }
        // A manifest that cannot be read is refused, never published as a
        // mystery blob: its frame list is what keeps the per-frame PNGs out
        // of the catalog.
        MediaKind::Billboard => {
            let bytes = read_all_from(handle, identity.len, &file.pack_path)?;
            let text = std::str::from_utf8(&bytes).map_err(|_| {
                PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{}: billboard manifest is not utf-8", file.pack_path),
                )
            })?;
            let bb = StatefulBillboard::parse(text).map_err(|e| {
                PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{}: {e}", file.pack_path),
                )
            })?;
            Ok(Measured { billboard: Some(bb), ..Measured::ok() })
        }
    }
}

fn probe_mp4_trusted(
    bytes: &[u8],
    digest: BlobId,
    pack_path: &str,
) -> Result<u32, PackImportError> {
    if BlobId::hash_of(bytes) != digest {
        return Err(PackImportError::new(
            PackImportErrorKind::Changed,
            format!("{pack_path}: probe bytes do not match digest"),
        ));
    }
    probe_mp4_verified(bytes, pack_path)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn probe_mp4_verified(_bytes: &[u8], _pack_path: &str) -> Result<u32, PackImportError> {
    Err(pack_os_unsupported())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn probe_mp4_verified(bytes: &[u8], pack_path: &str) -> Result<u32, PackImportError> {
    let hex = unix::random_hex16()?;
    let dir = std::env::temp_dir().join(format!(".pack-import-probe-{hex}"));
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        builder.mode(0o700);
    }
    match builder.create(&dir) {
        Ok(()) => {}
        Err(e) => {
            return Err(PackImportError::new(
                PackImportErrorKind::Io,
                format!("{pack_path}: create probe dir: {e}"),
            ))
        }
    }
    let path = dir.join("probe.mp4");
    let result = (|| -> Result<u32, PackImportError> {
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            opts.mode(0o600);
        }
        let mut file = opts.open(&path).map_err(|e| {
            PackImportError::new(
                PackImportErrorKind::Io,
                format!("{pack_path}: create probe leaf: {e}"),
            )
        })?;
        file.write_all(bytes).map_err(|e| {
            PackImportError::new(
                PackImportErrorKind::Io,
                format!("{pack_path}: write probe leaf: {e}"),
            )
        })?;
        file.sync_all().ok();
        drop(file);
        let probe = probe_video(&path).map_err(|e| {
            PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: {e}"),
            )
        })?;
        if !probe.real_frame || probe.duration_ms == 0 || probe.width == 0 || probe.height == 0 {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: undecodable or zero video"),
            ));
        }
        Ok(probe.duration_ms)
    })();
    let _ = fs::remove_dir_all(&dir);
    result
}

fn read_all_from(
    handle: &mut File,
    expected: u64,
    pack_path: &str,
) -> Result<Vec<u8>, PackImportError> {
    read_exact_capped(handle, expected, MAX_FILE_BYTES, pack_path)
}

fn read_exact_capped(
    handle: &mut File,
    expected: u64,
    max: u64,
    what: &str,
) -> Result<Vec<u8>, PackImportError> {
    if expected > max {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{what}: exceeds {max} bytes"),
        ));
    }
    let n = usize::try_from(expected).map_err(|_| {
        PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{what}: size overflow"),
        )
    })?;
    let mut bytes = vec![0u8; n];
    handle.read_exact(&mut bytes).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            PackImportError::new(
                PackImportErrorKind::Changed,
                format!("{what}: shrunk while reading"),
            )
        } else {
            PackImportError::new(PackImportErrorKind::Io, format!("{what}: {e}"))
        }
    })?;
    let mut extra = [0u8; 1];
    match handle.read(&mut extra) {
        Ok(0) => Ok(bytes),
        Ok(_) => Err(PackImportError::new(
            PackImportErrorKind::Changed,
            format!("{what}: grew while reading"),
        )),
        Err(e) => Err(PackImportError::new(
            PackImportErrorKind::Io,
            format!("{what}: {e}"),
        )),
    }
}

/// Signature + chunk walk with CRC, IHDR dims, IDAT, and IEND. No trailing bytes.
fn validate_png(bytes: &[u8], pack_path: &str) -> Result<(u32, u32), PackImportError> {
    const SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 8 || &bytes[..8] != SIG {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: not a PNG"),
        ));
    }
    let mut at = 8usize;
    let mut ihdr = None;
    let mut ihdr_meta = None;
    let mut idat = 0u32;
    let mut idat_bytes = 0usize;
    let mut iend = false;
    let mut chunks = 0u32;
    while at < bytes.len() {
        chunks = chunks.saturating_add(1);
        if chunks > 4096 {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: too many png chunks"),
            ));
        }
        if iend {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: trailing bytes after IEND"),
            ));
        }
        if at.checked_add(12).map(|n| n > bytes.len()).unwrap_or(true) {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: truncated png chunk"),
            ));
        }
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let typ = &bytes[at + 4..at + 8];
        let data_at = at + 8;
        let data_end = data_at.checked_add(len).ok_or_else(|| {
            PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: png chunk overflow"),
            )
        })?;
        let crc_end = data_end.checked_add(4).ok_or_else(|| {
            PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: png crc overflow"),
            )
        })?;
        if crc_end > bytes.len() {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: png chunk extends past EOF"),
            ));
        }
        let data = &bytes[data_at..data_end];
        let got = u32::from_be_bytes(bytes[data_end..crc_end].try_into().unwrap());
        let expect = crate::png::png_crc(&bytes[at + 4..data_end]);
        if got != expect {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: png crc mismatch"),
            ));
        }
        if at == 8 {
            if typ != b"IHDR" || len != 13 {
                return Err(PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{pack_path}: first png chunk must be IHDR"),
                ));
            }
            let w = u32::from_be_bytes(data[0..4].try_into().unwrap());
            let h = u32::from_be_bytes(data[4..8].try_into().unwrap());
            if w == 0 || h == 0 {
                return Err(PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{pack_path}: png has zero dimensions"),
                ));
            }
            if png_dims(&bytes[..24]).is_none() {
                return Err(PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{pack_path}: png header rejected"),
                ));
            }
            ihdr = Some((w, h));
            ihdr_meta = Some((data[8], data[9]));
        } else if typ == b"IDAT" {
            if data.is_empty() {
                return Err(PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{pack_path}: empty IDAT"),
                ));
            }
            idat_bytes = idat_bytes.saturating_add(data.len());
            if idat_bytes > MAX_FILE_BYTES as usize {
                return Err(PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{pack_path}: png IDAT exceeds budget"),
                ));
            }
            idat += 1;
        } else if typ == b"IEND" {
            if len != 0 {
                return Err(PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{pack_path}: IEND must be empty"),
                ));
            }
            iend = true;
        }
        at = crc_end;
    }
    let (w, h) = ihdr.ok_or_else(|| {
        PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: png missing IHDR"),
        )
    })?;
    if idat == 0 {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: png missing IDAT"),
        ));
    }
    if !iend {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: png missing IEND"),
        ));
    }
    let (bit_depth, color_type) = ihdr_meta.unwrap_or((8, 6));
    decode_png_pixels(bytes, w, h, bit_depth, color_type, pack_path)?;
    Ok((w, h))
}

/// Decode every pixel (inflate with its adler check, chunk CRCs, unfilter)
/// so a stream that only looks like a PNG is refused. The decoder sizes its
/// buffers from the header, so the decoded size is bounded FIRST, from the
/// IHDR alone: a header promising more than the file budget is refused
/// before anything is allocated (a decompression bomb).
fn decode_png_pixels(
    bytes: &[u8],
    w: u32,
    h: u32,
    bit_depth: u8,
    color_type: u8,
    pack_path: &str,
) -> Result<(), PackImportError> {
    use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
    use makepad_zune_png::makepad_zune_core::options::DecoderOptions;
    let samples: u64 = match color_type {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: invalid png color type"),
            ))
        }
    };
    let row_bytes = (u64::from(w) * samples * u64::from(bit_depth)).div_ceil(8) + 1;
    let decoded = row_bytes.saturating_mul(u64::from(h));
    if decoded == 0 || decoded > MAX_FILE_BYTES {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: png decode budget"),
        ));
    }
    // The budget above is the bound; a strip wider than the decoder's
    // default side limit is fine inside it.
    let options = DecoderOptions::default()
        .set_max_width(1 << 24)
        .set_max_height(1 << 24)
        .inflate_set_confirm_adler(true)
        .png_set_confirm_crc(true);
    let mut decoder = makepad_zune_png::PngDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder.decode_raw().map(|_| ()).map_err(|e| {
        PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: png decode: {e:?}"),
        )
    })
}

const KTX2_IDENTIFIER: [u8; 12] = [0xAB, b'K', b'T', b'X', b' ', b'2', b'0', 0xBB, b'\r', b'\n', 0x1A, b'\n'];

/// Dimensions of an embedded KTX2 texture (glTF `KHR_texture_basisu`, as
/// Basis Universal images carry it): the fixed 80-byte header must be
/// present, `vkFormat` must be 0 (a supercompressed/Basis payload), and the
/// base level must have a size.
fn validate_ktx2(bytes: &[u8], pack_path: &str) -> Result<(u32, u32), PackImportError> {
    let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    if bytes.len() < 80 || !bytes.starts_with(&KTX2_IDENTIFIER) {
        return Err(PackImportError::new(PackImportErrorKind::Malformed, format!("{pack_path}: not a ktx2 texture")));
    }
    // Basis (vkFormat 0), or BC5 (141): load-ready normal maps.
    if word(12) != 0 && word(12) != 141 {
        return Err(PackImportError::new(PackImportErrorKind::Malformed, format!("{pack_path}: ktx2 texture is not Basis or BC5 (vkFormat {})", word(12))));
    }
    let (w, h) = (word(20), word(24));
    if w == 0 || h == 0 {
        return Err(PackImportError::new(PackImportErrorKind::Malformed, format!("{pack_path}: ktx2 texture has no size")));
    }
    Ok((w, h))
}

fn validate_jpeg(bytes: &[u8], pack_path: &str) -> Result<(u32, u32), PackImportError> {
    if bytes.len() < 4 || bytes[0] != 0xff || bytes[1] != 0xd8 {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: not a jpeg"),
        ));
    }
    let mut at = 2usize;
    let mut sof = None;
    let mut sof_marker = 0u8;
    let mut sof_comps: Vec<(u8, u8, u8)> = Vec::new();
    let mut dht = vec![None; 8];
    let mut sos = false;
    let mut sos_comps: Vec<(u8, u8)> = Vec::new();
    let mut scan_range: Option<(usize, usize)> = None;
    let mut eoi = false;
    let mut markers = 0u32;
    while at < bytes.len() {
        markers = markers.saturating_add(1);
        if markers > 65_536 {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: too many jpeg markers"),
            ));
        }
        if eoi {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: trailing bytes after jpeg EOI"),
            ));
        }
        if bytes[at] != 0xff {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: jpeg marker desync"),
            ));
        }
        while at < bytes.len() && bytes[at] == 0xff {
            at += 1;
        }
        if at >= bytes.len() {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: truncated jpeg marker"),
            ));
        }
        let marker = bytes[at];
        at += 1;
        if marker == 0xd8 {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: extra jpeg SOI"),
            ));
        }
        if marker == 0xd9 {
            eoi = true;
            continue;
        }
        if (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
            continue;
        }
        if at.checked_add(2).map(|n| n > bytes.len()).unwrap_or(true) {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: truncated jpeg segment"),
            ));
        }
        let len = u16::from_be_bytes([bytes[at], bytes[at + 1]]) as usize;
        if len < 2 {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: jpeg segment length"),
            ));
        }
        let data_end = at.checked_add(len).ok_or_else(|| {
            PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: jpeg segment overflow"),
            )
        })?;
        if data_end > bytes.len() {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: jpeg segment extends past EOF"),
            ));
        }
        if matches!(
            marker,
            0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf
        ) {
            if len < 8 {
                return Err(PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{pack_path}: truncated jpeg SOF"),
                ));
            }
            let h = u16::from_be_bytes([bytes[at + 3], bytes[at + 4]]) as u32;
            let w = u16::from_be_bytes([bytes[at + 5], bytes[at + 6]]) as u32;
            if w == 0 || h == 0 {
                return Err(PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{pack_path}: jpeg has zero dimensions"),
                ));
            }
            sof = Some((w, h));
            sof_marker = marker;
            let nf = bytes[at + 7] as usize;
            if len < 8 + 3 * nf || nf == 0 || nf > 4 {
                return Err(PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{pack_path}: jpeg SOF components"),
                ));
            }
            sof_comps.clear();
            for i in 0..nf {
                let id = bytes[at + 8 + i * 3];
                let hv = bytes[at + 9 + i * 3];
                sof_comps.push((id, hv >> 4, hv & 0x0f));
            }
        }
        if marker == 0xc4 {
            parse_dht(&bytes[at + 2..data_end], &mut dht, pack_path)?;
        }
        if marker == 0xda {
            sos = true;
            if data_end - at < 3 {
                return Err(PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{pack_path}: truncated jpeg SOS"),
                ));
            }
            let ns = bytes[at + 2] as usize;
            if ns == 0 || ns > 4 || len < 6 + 2 * ns {
                return Err(PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{pack_path}: jpeg SOS components"),
                ));
            }
            sos_comps.clear();
            for i in 0..ns {
                sos_comps.push((bytes[at + 3 + i * 2], bytes[at + 4 + i * 2]));
            }
            let scan_start = data_end;
            at = data_end;
            loop {
                if at >= bytes.len() {
                    return Err(PackImportError::new(
                        PackImportErrorKind::Malformed,
                        format!("{pack_path}: truncated jpeg scan"),
                    ));
                }
                if bytes[at] != 0xff {
                    at += 1;
                    continue;
                }
                if at + 1 >= bytes.len() {
                    return Err(PackImportError::new(
                        PackImportErrorKind::Malformed,
                        format!("{pack_path}: truncated jpeg entropy marker"),
                    ));
                }
                let next = bytes[at + 1];
                if next == 0x00 || (0xd0..=0xd7).contains(&next) {
                    at += 2;
                    continue;
                }
                if next == 0xff {
                    at += 1;
                    continue;
                }
                break;
            }
            scan_range = Some((scan_start, at));
            continue;
        }
        at = data_end;
    }
    let (w, h) = sof.ok_or_else(|| {
        PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: jpeg missing SOF"),
        )
    })?;
    if !sos {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: jpeg missing SOS"),
        ));
    }
    if !eoi {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: jpeg missing EOI"),
        ));
    }
    if jpeg_dims(bytes) != Some((w, h)) {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: jpeg dimensions disagree"),
        ));
    }
    if !matches!(sof_marker, 0xc0 | 0xc1) {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: jpeg SOF is not sequential baseline"),
        ));
    }
    let (scan_lo, scan_hi) = scan_range.ok_or_else(|| {
        PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: jpeg missing scan"),
        )
    })?;
    decode_jpeg_entropy(
        &bytes[scan_lo..scan_hi],
        w,
        h,
        &sof_comps,
        &sos_comps,
        &dht,
        pack_path,
    )?;
    Ok((w, h))
}

fn parse_dht(
    data: &[u8],
    tables: &mut [Option<JpegHuff>],
    pack_path: &str,
) -> Result<(), PackImportError> {
    let mut i = 0usize;
    while i < data.len() {
        if i + 17 > data.len() {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: truncated jpeg DHT"),
            ));
        }
        let tc_th = data[i];
        let class = tc_th >> 4;
        let dest = tc_th & 0x0f;
        if class > 1 || dest > 3 {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: jpeg DHT table id"),
            ));
        }
        i += 1;
        let mut counts = [0u8; 16];
        counts.copy_from_slice(&data[i..i + 16]);
        i += 16;
        let nsym: usize = counts.iter().map(|&c| c as usize).sum();
        if i + nsym > data.len() {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: truncated jpeg DHT symbols"),
            ));
        }
        let symbols = data[i..i + nsym].to_vec();
        i += nsym;
        tables[(class * 4 + dest) as usize] = Some(build_jpeg_huff(&counts, &symbols, pack_path)?);
    }
    Ok(())
}

#[derive(Clone)]
struct JpegHuff {
    entries: Vec<(u8, u8, u16)>,
    max_bits: u8,
    /// Per code length: the first (canonical) code, the index of its entry
    /// and how many codes have that length. A decode indexes these instead
    /// of scanning every entry per bit, which was ~90% of a Doom pack's
    /// compile (5.5 s of JPEG validation).
    first: [u16; 17],
    offset: [u16; 17],
    count: [u8; 17],
}

fn build_jpeg_huff(
    counts: &[u8; 16],
    symbols: &[u8],
    pack_path: &str,
) -> Result<JpegHuff, PackImportError> {
    let mut entries = Vec::new();
    let mut max_bits = 0u8;
    let mut code = 0u16;
    let mut si = 0usize;
    let (mut first, mut offset, mut count) = ([0u16; 17], [0u16; 17], [0u8; 17]);
    for bits in 1..=16 {
        (first[bits], offset[bits], count[bits]) = (code, entries.len() as u16, counts[bits - 1]);
        for _ in 0..counts[bits - 1] {
            let sym = *symbols.get(si).ok_or_else(|| {
                PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{pack_path}: jpeg DHT symbol"),
                )
            })?;
            si += 1;
            max_bits = max_bits.max(bits as u8);
            entries.push((sym, bits as u8, code));
            code = code.wrapping_add(1);
        }
        code <<= 1;
    }
    Ok(JpegHuff { entries, max_bits, first, offset, count })
}

struct JpegBits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u8,
    nbits: u8,
}

impl<'a> JpegBits<'a> {
    fn bit(&mut self) -> Result<u8, &'static str> {
        if self.nbits == 0 {
            let b = *self.data.get(self.pos).ok_or("truncated jpeg entropy")?;
            self.pos += 1;
            if b == 0xff {
                let n = *self.data.get(self.pos).ok_or("truncated jpeg stuff")?;
                self.pos += 1;
                if n == 0x00 {
                    self.buf = 0xff;
                } else if (0xd0..=0xd7).contains(&n) {
                    self.buf = 0;
                    self.nbits = 0;
                    return self.bit();
                } else {
                    return Err("unexpected jpeg marker in scan");
                }
            } else {
                self.buf = b;
            }
            self.nbits = 8;
        }
        self.nbits -= 1;
        Ok((self.buf >> self.nbits) & 1)
    }

    fn bits(&mut self, n: u32) -> Result<u16, &'static str> {
        let mut v = 0u16;
        for _ in 0..n {
            v = (v << 1) | u16::from(self.bit()?);
        }
        Ok(v)
    }
}

fn decode_jpeg_huff(br: &mut JpegBits, table: &JpegHuff) -> Result<u8, &'static str> {
    let mut code = 0u16;
    for bits in 1..=table.max_bits as usize {
        code = (code << 1) | u16::from(br.bit()?);
        let index = code.wrapping_sub(table.first[bits]);
        if index < u16::from(table.count[bits]) {
            return Ok(table.entries[(table.offset[bits] + index) as usize].0);
        }
    }
    Err("bad jpeg huffman")
}

fn decode_jpeg_entropy(
    scan: &[u8],
    w: u32,
    h: u32,
    sof_comps: &[(u8, u8, u8)],
    sos_comps: &[(u8, u8)],
    dht: &[Option<JpegHuff>],
    pack_path: &str,
) -> Result<(), PackImportError> {
    if scan.is_empty() {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: jpeg scan is empty"),
        ));
    }
    let mut max_h = 1u32;
    let mut max_v = 1u32;
    for &(_, hs, vs) in sof_comps {
        if hs == 0 || vs == 0 {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: jpeg sampling"),
            ));
        }
        max_h = max_h.max(u32::from(hs));
        max_v = max_v.max(u32::from(vs));
    }
    let mcu_w = 8 * max_h;
    let mcu_h = 8 * max_v;
    let mcus_x = (w + mcu_w - 1) / mcu_w;
    let mcus_y = (h + mcu_h - 1) / mcu_h;
    let mut br = JpegBits {
        data: scan,
        pos: 0,
        buf: 0,
        nbits: 0,
    };
    let err = |e: &'static str| {
        PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: jpeg decode: {e}"),
        )
    };
    for _my in 0..mcus_y {
        for _mx in 0..mcus_x {
            for &(cid, tdta) in sos_comps {
                let (_, hs, vs) = sof_comps
                    .iter()
                    .copied()
                    .find(|(id, _, _)| *id == cid)
                    .ok_or_else(|| err("SOS component missing from SOF"))?;
                let dc = dht
                    .get(((tdta >> 4) & 0x0f) as usize)
                    .and_then(|t| t.as_ref())
                    .ok_or_else(|| err("missing DC table"))?;
                let ac = dht
                    .get((4 + (tdta & 0x0f)) as usize)
                    .and_then(|t| t.as_ref())
                    .ok_or_else(|| err("missing AC table"))?;
                for _ in 0..(hs as u32 * vs as u32) {
                    let ssss = decode_jpeg_huff(&mut br, dc).map_err(err)?;
                    if ssss > 11 {
                        return Err(err("DC ssss"));
                    }
                    let _ = br.bits(u32::from(ssss)).map_err(err)?;
                    let mut k = 1u8;
                    while k < 64 {
                        let rs = decode_jpeg_huff(&mut br, ac).map_err(err)?;
                        if rs == 0x00 {
                            break;
                        }
                        if rs == 0xf0 {
                            k = k.saturating_add(16);
                            continue;
                        }
                        let r = rs >> 4;
                        let s = rs & 0x0f;
                        k = k.saturating_add(r);
                        if s == 0 || k >= 64 {
                            return Err(err("AC run"));
                        }
                        let _ = br.bits(u32::from(s)).map_err(err)?;
                        k = k.saturating_add(1);
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_wav(bytes: &[u8], pack_path: &str) -> Result<u32, PackImportError> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: not a RIFF/WAVE file"),
        ));
    }
    let mut at = 12usize;
    while at < bytes.len() {
        if at.checked_add(8).map(|n| n > bytes.len()).unwrap_or(true) {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: truncated wav chunk header"),
            ));
        }
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = at + 8;
        let end = body.checked_add(size).ok_or_else(|| {
            PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: wav chunk overflow"),
            )
        })?;
        if end > bytes.len() {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: wav chunk extends past EOF"),
            ));
        }
        at = end + (size & 1);
        if at > bytes.len() {
            // RIFF pads an odd-sized chunk to even, but the pad byte after
            // the LAST chunk is routinely left off — the file simply ends
            // where the data ends. Nothing is missing: `end` already proved
            // the chunk body is entirely present, and there is no following
            // chunk that the missing byte could misalign. Every practical
            // reader accepts this; refusing it cost the whole Quake 3
            // import for one sound (`sound/world/drone6.wav`, an 18329-byte
            // data chunk ending exactly at EOF).
            if end == bytes.len() {
                break;
            }
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: wav pad extends past EOF"),
            ));
        }
    }
    let pcm = parse_wav(bytes).map_err(|e| {
        PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: {e}"),
        )
    })?;
    let millis = pcm.millis();
    if millis == 0 {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: wav duration is 0"),
        ));
    }
    Ok(millis)
}

fn validate_mp4_structure(bytes: &[u8], pack_path: &str) -> Result<(), PackImportError> {
    let mut saw_mvhd = false;
    let mut saw_vide = false;
    let mut saw_sample = false;
    walk_mp4_boxes(bytes, 0, bytes.len(), 0, pack_path, &mut saw_mvhd, &mut saw_vide, &mut saw_sample)?;
    if !saw_mvhd {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: mp4 missing mvhd"),
        ));
    }
    if !saw_vide || !saw_sample {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: mp4 missing video track/sample table"),
        ));
    }
    Ok(())
}

fn walk_mp4_boxes(
    bytes: &[u8],
    start: usize,
    end: usize,
    depth: u32,
    pack_path: &str,
    saw_mvhd: &mut bool,
    saw_vide: &mut bool,
    saw_sample: &mut bool,
) -> Result<(), PackImportError> {
    if depth > MAX_MP4_BOX_DEPTH {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: mp4 box nesting exceeds {MAX_MP4_BOX_DEPTH}"),
        ));
    }
    if start > end || end > bytes.len() {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: mp4 box range overflow"),
        ));
    }
    let mut at = start;
    while at.checked_add(8).map(|n| n <= end).unwrap_or(false) {
        let size32 = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as u64;
        let kind = &bytes[at + 4..at + 8];
        let (hdr, box_size) = if size32 == 1 {
            if at.checked_add(16).map(|n| n > end).unwrap_or(true) {
                return Err(PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{pack_path}: truncated mp4 largesize"),
                ));
            }
            let size = u64::from_be_bytes(bytes[at + 8..at + 16].try_into().unwrap());
            (16u64, size)
        } else if size32 == 0 {
            (
                8u64,
                u64::try_from(end.checked_sub(at).unwrap_or(0)).unwrap_or(u64::MAX),
            )
        } else {
            (8u64, size32)
        };
        if box_size < hdr {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: mp4 box smaller than header"),
            ));
        }
        let box_end = (at as u64)
            .checked_add(box_size)
            .ok_or_else(|| {
                PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{pack_path}: mp4 box size overflow"),
                )
            })?;
        if box_end > end as u64 {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: mp4 box extends past parent"),
            ));
        }
        let body = at + hdr as usize;
        let body_end = box_end as usize;
        if kind == b"mvhd" {
            *saw_mvhd = true;
            if body_end.saturating_sub(body) < 20 {
                return Err(PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{pack_path}: truncated mvhd"),
                ));
            }
        }
        if kind == b"hdlr" && body + 16 <= body_end && &bytes[body + 8..body + 12] == b"vide" {
            *saw_vide = true;
        }
        if kind == b"stsz" || kind == b"stz2" || kind == b"stts" {
            *saw_sample = true;
        }
        if matches!(
            kind,
            b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"edts"
        ) {
            walk_mp4_boxes(
                bytes,
                body,
                body_end,
                depth + 1,
                pack_path,
                saw_mvhd,
                saw_vide,
                saw_sample,
            )?;
        }
        at = body_end;
    }
    if at != end && at < end && end - at < 8 {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: truncated mp4 box"),
        ));
    }
    Ok(())
}

fn measure_glb(bytes: &[u8], pack_path: &str) -> Result<GlbMeasure, PackImportError> {
    let image_uris = preflight_glb(bytes, pack_path)?;
    let vehicle_anchors = inspect_vehicle_connections(bytes, pack_path)?;
    let static_res = StaticModel::parse_glb(bytes);
    let inspect_res = inspect_glb(bytes);
    let skinned_res = SkinnedModel::parse_glb(bytes);
    let model = match static_res {
        Ok(m) => m,
        Err(static_err) => {
            let detail = inspect_res
                .as_ref()
                .err()
                .map(|s| s.as_str())
                .or_else(|| skinned_res.as_ref().err().map(|s| s.as_str()))
                .unwrap_or(static_err.as_str());
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: {detail}"),
            ));
        }
    };
    // StaticModel deliberately removes externally-driven rigid parts from
    // its flattened body stream. Catalog metrics describe the complete GLB,
    // so prefer inspect_glb's document topology (body + driven parts).
    let triangles = inspect_res
        .as_ref()
        .map(|m| m.triangles)
        .unwrap_or_else(|_| model.triangle_count() as u32);
    let vertices = inspect_res
        .as_ref()
        .map(|m| m.vertices)
        .unwrap_or_else(|_| model.vertex_count() as u32);
    if triangles == 0 || vertices < 3 {
        return Err(PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: unmeasured mesh metrics"),
        ));
    }
    if triangles > MAX_TRIANGLES || vertices > MAX_VERTICES {
        return Err(PackImportError::new(
            PackImportErrorKind::Content,
            format!("{pack_path}: mesh exceeds peak triangle/vertex budget"),
        ));
    }
    let bounds = Bounds {
        min: Vec3::new(model.min.x, model.min.y, model.min.z),
        max: Vec3::new(model.max.x, model.max.y, model.max.z),
    };
    bounds.validate_pack(pack_path)?;
    let mut joints = 0u16;
    let mut clips = 0u16;
    if let Ok(skin) = skinned_res {
        joints = skin.joint_count().min(u16::MAX as usize) as u16;
        clips = skin.clips.len().min(u16::MAX as usize) as u16;
    }
    if let Ok(inspect) = inspect_res {
        if inspect.joints > joints {
            joints = inspect.joints;
        }
        if inspect.clips > clips {
            clips = inspect.clips;
        }
    }
    if joints > MAX_JOINTS || clips > MAX_CLIPS {
        return Err(PackImportError::new(
            PackImportErrorKind::Content,
            format!("{pack_path}: mesh exceeds peak joint/clip budget"),
        ));
    }
    let rigged = joints > 0;
    let animated = clips > 0;
    // Classic Freedoom/LibreQuake staging uses path prefixes (worlds/, weapons/,
    // props/). Kenney packs never use those folders — additive only.
    let kind = classic_glb_kind(pack_path).unwrap_or_else(|| {
        if vehicle_anchors.is_some() {
            AssetKind::Vehicle
        } else if rigged && animated {
            AssetKind::Character
        } else {
            AssetKind::Mesh
        }
    });
    let mut max_texture_dim = 0u32;
    if let Some(img) = model.texture_png.as_deref() {
        let (w, h) = if img.starts_with(b"\x89PNG\r\n\x1a\n") {
            validate_png(img, pack_path)?
        } else if img.starts_with(&KTX2_IDENTIFIER) {
            validate_ktx2(img, pack_path)?
        } else {
            validate_jpeg(img, pack_path)?
        };
        if w > MAX_TEXTURE_DIM || h > MAX_TEXTURE_DIM {
            return Err(PackImportError::new(
                PackImportErrorKind::Content,
                format!("{pack_path}: embedded texture exceeds MAX_TEXTURE_DIM"),
            ));
        }
        max_texture_dim = w.max(h);
    }
    Ok(GlbMeasure {
        kind,
        triangles,
        vertices,
        joints,
        clips,
        max_texture_dim,
        bounds,
        rigged,
        animated,
        image_uris,
        anchors: vehicle_anchors.unwrap_or_default(),
    })
}

fn glb_err(pack_path: &str, msg: impl Into<String>) -> PackImportError {
    PackImportError::new(
        PackImportErrorKind::Malformed,
        format!("{pack_path}: {}", msg.into()),
    )
}

struct GlbContainer<'a> {
    json: &'a [u8],
    bin: &'a [u8],
}

/// Strict GLB container: exact header length, padded chunks, no trailing
/// bytes, no duplicate JSON/BIN. Must run before any runtime parser.
fn parse_glb_container<'a>(
    bytes: &'a [u8],
    pack_path: &str,
) -> Result<GlbContainer<'a>, PackImportError> {
    if bytes.len() < 20 || &bytes[0..4] != b"glTF" {
        return Err(glb_err(pack_path, "not a GLB"));
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    if version != 2 {
        return Err(glb_err(pack_path, format!("unsupported GLB version {version}")));
    }
    let total = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    if total != bytes.len() {
        return Err(glb_err(
            pack_path,
            format!("GLB length {} does not match file {}", total, bytes.len()),
        ));
    }
    let mut at = 12usize;
    let mut json: Option<&[u8]> = None;
    let mut bin: Option<&[u8]> = None;
    let mut chunks = 0usize;
    while at < bytes.len() {
        chunks += 1;
        if chunks > MAX_GLB_CHUNKS {
            return Err(glb_err(
                pack_path,
                format!("GLB chunk count exceeds {MAX_GLB_CHUNKS}"),
            ));
        }
        let hdr_end = at.checked_add(8).ok_or_else(|| glb_err(pack_path, "GLB chunk header overflow"))?;
        if hdr_end > bytes.len() {
            return Err(glb_err(pack_path, "truncated GLB chunk header"));
        }
        let chunk_len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let kind = &bytes[at + 4..at + 8];
        let data_end = hdr_end
            .checked_add(chunk_len)
            .ok_or_else(|| glb_err(pack_path, "GLB chunk overflow"))?;
        if data_end > bytes.len() {
            return Err(glb_err(pack_path, "GLB chunk extends past EOF"));
        }
        let pad = (4 - (chunk_len % 4)) % 4;
        let padded_end = data_end
            .checked_add(pad)
            .ok_or_else(|| glb_err(pack_path, "GLB chunk pad overflow"))?;
        if padded_end > bytes.len() {
            return Err(glb_err(pack_path, "GLB chunk padding extends past EOF"));
        }
        let data = &bytes[hdr_end..data_end];
        match kind {
            b"JSON" => {
                if json.is_some() {
                    return Err(glb_err(pack_path, "duplicate GLB JSON chunk"));
                }
                json = Some(data);
            }
            b"BIN\0" => {
                if bin.is_some() {
                    return Err(glb_err(pack_path, "duplicate GLB BIN chunk"));
                }
                bin = Some(data);
            }
            _ => {
                return Err(glb_err(
                    pack_path,
                    format!("unsupported GLB chunk {}", String::from_utf8_lossy(kind)),
                ))
            }
        }
        at = padded_end;
    }
    if at != bytes.len() {
        return Err(glb_err(pack_path, "GLB trailing bytes"));
    }
    let json = json.ok_or_else(|| glb_err(pack_path, "GLB missing JSON chunk"))?;
    Ok(GlbContainer {
        json,
        bin: bin.unwrap_or(&[]),
    })
}

fn json_u64(v: &Value) -> Option<u64> {
    match v {
        Value::Int(i) if *i >= 0 => Some(*i as u64),
        Value::F64(f) if f.is_finite() && *f >= 0.0 && f.fract() == 0.0 && *f < (1u64 << 53) as f64 => {
            Some(*f as u64)
        }
        _ => None,
    }
}

fn json_opt_u64(obj: &Value, key: &str) -> Result<Option<u64>, PackImportError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => json_u64(v).map(Some).ok_or_else(|| {
            PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("glb {key} is not a non-negative integer"),
            )
        }),
    }
}

fn json_req_u64(obj: &Value, key: &str, pack_path: &str) -> Result<u64, PackImportError> {
    json_opt_u64(obj, key)?.ok_or_else(|| glb_err(pack_path, format!("glb missing {key}")))
}

fn glb_type_lanes(ty: &str) -> Option<u64> {
    Some(match ty {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" => 4,
        "MAT2" => 4,
        "MAT3" => 9,
        "MAT4" => 16,
        _ => return None,
    })
}

fn glb_component_size(ct: u64) -> Option<u64> {
    Some(match ct {
        5120 | 5121 => 1,
        5122 | 5123 => 2,
        5125 | 5126 => 4,
        _ => return None,
    })
}

/// Mandatory GLB preflight before StaticModel / SkinnedModel / inspect_glb:
/// container, URIs, accessor budgets/overflow, node graph cycle/depth.
fn preflight_glb(bytes: &[u8], pack_path: &str) -> Result<Vec<String>, PackImportError> {
    let container = parse_glb_container(bytes, pack_path)?;
    let value = json::parse(container.json).map_err(|e| {
        PackImportError::new(
            PackImportErrorKind::Malformed,
            format!("{pack_path}: GLB JSON: {e}"),
        )
    })?;
    let image_uris = preflight_glb_uris_from_json(&value, pack_path)?;
    preflight_glb_accessors(&value, container.bin.len() as u64, pack_path)?;
    preflight_glb_nodes(&value, pack_path)?;
    preflight_glb_meshes(&value, pack_path)?;
    Ok(image_uris)
}

fn preflight_glb_uris_from_json(
    value: &Value,
    pack_path: &str,
) -> Result<Vec<String>, PackImportError> {
    if let Some(arr) = value.get("buffers").and_then(|v| v.as_arr()) {
        if arr.len() > 8 {
            return Err(glb_err(pack_path, "too many glb buffers"));
        }
        for buf in arr {
            if let Some(uri) = buf.get("uri").and_then(|u| u.as_str()) {
                refuse_external_uri(uri, pack_path)?;
                return Err(glb_err(
                    pack_path,
                    "glb buffer uri is not a self-contained BIN chunk",
                ));
            }
        }
    }
    let mut image_uris = Vec::new();
    let mut embedded = 0u32;
    if let Some(arr) = value.get("images").and_then(|v| v.as_arr()) {
        // World/map GLBs (Quake, Duke levels) embed every surface texture;
        // the cap only guards against runaway memory, not real content.
        if arr.len() > 256 {
            return Err(glb_err(pack_path, "too many glb images"));
        }
        for img in arr {
            match (
                img.get("uri").and_then(|u| u.as_str()),
                img.get("bufferView"),
            ) {
                (Some(uri), _) => {
                    refuse_external_uri(uri, pack_path)?;
                    image_uris.push(uri.to_string());
                }
                (None, Some(_)) => embedded = embedded.saturating_add(1),
                (None, None) => {
                    return Err(glb_err(
                        pack_path,
                        "glb image has neither uri nor bufferView",
                    ))
                }
            }
        }
    }
    // The one-atlas rule is about texture FILES this pack must publish as
    // their own blobs: a mesh asset carries a single `FileRole::Texture`
    // (`mesh_asset`'s `albedo`), so a GLB pointing at two pack images has
    // nowhere to put the second.
    //
    // An EMBEDDED image is not that. It already lives in the BIN chunk of
    // the mesh blob, costs the manifest nothing, and both render lanes draw
    // it: `StaticModel::split_draw_layers` emits one draw layer per
    // embedded base-color image, and `GltfRenderer::apply_material` binds
    // per-material textures for one draw per primitive.
    //
    // Counting embedded images here refused every multi-material model a
    // vendor ships — 157 of them across Kenney's two retro kits, whole kits
    // lost for it — AND every level this repo's own
    // `write_glb_mesh_textured_parts` writes, which is one image per
    // surface by construction.
    //
    // A sky node paints itself from its own picture and a prelit marker
    // points back at an existing one: neither makes a level's ONE atlas
    // into two, which is what `annexed` still discounts for the mixed case.
    let annexed = annexed_images(value);
    let embedded = embedded.saturating_sub(annexed);
    if image_uris.len() > 1 || (image_uris.len() == 1 && embedded > 0) {
        // `Unsupported`, not `Malformed`: the file is fine, this importer
        // just has one albedo slot to put it in. That kind is what lets a
        // single such model be skipped instead of costing the whole pack.
        return Err(PackImportError::new(
            PackImportErrorKind::Unsupported,
            format!("{pack_path}: unsupported multi-texture glb"),
        ));
    }
    Ok(image_uris)
}

/// Images that belong to a `sky` node's own material — they are not the
/// mesh's texture and do not count toward the one-atlas rule.
fn annexed_images(value: &Value) -> u32 {
    let arr = |name: &str| value.get(name).and_then(|v| v.as_arr()).unwrap_or(&[]);
    let materials = arr("materials");
    let textures = arr("textures");
    let meshes = arr("meshes");
    let mut annexed: BTreeSet<u64> = BTreeSet::new();
    let image_of_material = |mi: usize| -> Option<u64> {
        let ti = materials
            .get(mi)?
            .get("pbrMetallicRoughness")?
            .get("baseColorTexture")?
            .get("index")
            .and_then(json_u64)?;
        textures
            .get(ti as usize)?
            .get("source")
            .and_then(json_u64)
    };
    for node in arr("nodes") {
        let is_sky = node
            .get("extras")
            .and_then(|e| e.get("kind"))
            .and_then(|k| k.as_str())
            == Some("sky");
        if !is_sky {
            continue;
        }
        let Some(mesh) = node.get("mesh").and_then(json_u64) else {
            continue;
        };
        let Some(prims) = meshes
            .get(mesh as usize)
            .and_then(|m| m.get("primitives"))
            .and_then(|p| p.as_arr())
        else {
            continue;
        };
        for prim in prims {
            if let Some(mi) = prim.get("material").and_then(json_u64) {
                if let Some(image) = image_of_material(mi as usize) {
                    annexed.insert(image);
                }
            }
        }
    }
    annexed.len() as u32
}

fn preflight_glb_accessors(
    value: &Value,
    bin_len: u64,
    pack_path: &str,
) -> Result<(), PackImportError> {
    let views = match value.get("bufferViews") {
        None | Some(Value::Null) => &[][..],
        Some(v) => v
            .as_arr()
            .ok_or_else(|| glb_err(pack_path, "glb bufferViews must be an array"))?,
    };
    if views.len() > MAX_GLB_BUFFER_VIEWS {
        return Err(glb_err(
            pack_path,
            format!("glb bufferViews exceed {MAX_GLB_BUFFER_VIEWS}"),
        ));
    }
    let mut view_spans = Vec::with_capacity(views.len());
    for view in views {
        let offset = json_opt_u64(view, "byteOffset")?.unwrap_or(0);
        let length = json_req_u64(view, "byteLength", pack_path)?;
        let end = offset
            .checked_add(length)
            .ok_or_else(|| glb_err(pack_path, "glb bufferView offset overflow"))?;
        if end > bin_len {
            return Err(glb_err(pack_path, "glb bufferView extends past BIN"));
        }
        if let Some(stride) = json_opt_u64(view, "byteStride")? {
            if stride == 0 || stride > 255 {
                return Err(glb_err(pack_path, "glb bufferView byteStride"));
            }
        }
        view_spans.push((offset, length));
    }
    let accessors = match value.get("accessors") {
        None | Some(Value::Null) => return Ok(()),
        Some(v) => v
            .as_arr()
            .ok_or_else(|| glb_err(pack_path, "glb accessors must be an array"))?,
    };
    if accessors.len() > MAX_GLB_ACCESSORS {
        return Err(glb_err(
            pack_path,
            format!("glb accessors exceed {MAX_GLB_ACCESSORS}"),
        ));
    }
    for (i, acc) in accessors.iter().enumerate() {
        let count = json_req_u64(acc, "count", pack_path)?;
        let ct = json_req_u64(acc, "componentType", pack_path)?;
        let ty = acc
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| glb_err(pack_path, format!("glb accessor {i} missing type")))?;
        let lanes = glb_type_lanes(ty).ok_or_else(|| {
            glb_err(pack_path, format!("glb accessor {i} unsupported type {ty}"))
        })?;
        let csize = glb_component_size(ct).ok_or_else(|| {
            glb_err(
                pack_path,
                format!("glb accessor {i} unsupported componentType {ct}"),
            )
        })?;
        let elem = lanes
            .checked_mul(csize)
            .ok_or_else(|| glb_err(pack_path, format!("glb accessor {i} element size overflow")))?;
        let packed = count
            .checked_mul(elem)
            .ok_or_else(|| glb_err(pack_path, format!("glb accessor {i} count*lanes overflow")))?;
        if count > MAX_VERTICES as u64 {
            return Err(PackImportError::new(
                PackImportErrorKind::Content,
                format!("{pack_path}: glb accessor {i} count {count} exceeds MAX_VERTICES"),
            ));
        }
        if packed > MAX_FILE_BYTES {
            return Err(PackImportError::new(
                PackImportErrorKind::Content,
                format!("{pack_path}: glb accessor {i} payload exceeds MAX_FILE_BYTES"),
            ));
        }
        let acc_off = json_opt_u64(acc, "byteOffset")?.unwrap_or(0);
        if let Some(vi) = json_opt_u64(acc, "bufferView")? {
            let idx = usize::try_from(vi)
                .ok()
                .filter(|i| *i < view_spans.len())
                .ok_or_else(|| glb_err(pack_path, format!("glb accessor {i} bad bufferView")))?;
            let (view_off, view_len) = view_spans[idx];
            let stride = match value
                .get("bufferViews")
                .and_then(|v| v.as_arr())
                .and_then(|a| a.get(idx))
            {
                Some(view) => json_opt_u64(view, "byteStride")?.unwrap_or(elem),
                None => elem,
            };
            if stride < elem {
                return Err(glb_err(pack_path, format!("glb accessor {i} stride < element")));
            }
            let last = if count == 0 {
                acc_off
            } else {
                let span = (count - 1)
                    .checked_mul(stride)
                    .and_then(|s| s.checked_add(elem))
                    .and_then(|s| s.checked_add(acc_off))
                    .ok_or_else(|| glb_err(pack_path, format!("glb accessor {i} range overflow")))?;
                span
            };
            if last > view_len {
                return Err(glb_err(
                    pack_path,
                    format!("glb accessor {i} extends past bufferView"),
                ));
            }
            let abs = view_off
                .checked_add(last)
                .ok_or_else(|| glb_err(pack_path, format!("glb accessor {i} bin overflow")))?;
            if abs > bin_len {
                return Err(glb_err(pack_path, format!("glb accessor {i} extends past BIN")));
            }
        }
    }
    Ok(())
}

fn preflight_glb_nodes(value: &Value, pack_path: &str) -> Result<(), PackImportError> {
    let nodes = match value.get("nodes") {
        None | Some(Value::Null) => return Ok(()),
        Some(v) => v
            .as_arr()
            .ok_or_else(|| glb_err(pack_path, "glb nodes must be an array"))?,
    };
    if nodes.len() > MAX_GLB_NODES {
        return Err(glb_err(pack_path, format!("glb nodes exceed {MAX_GLB_NODES}")));
    }
    let n = nodes.len();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, node) in nodes.iter().enumerate() {
        if let Some(mesh) = json_opt_u64(node, "mesh")? {
            if mesh > MAX_GLB_MESHES as u64 {
                return Err(glb_err(pack_path, format!("glb node {i} mesh index")));
            }
        }
        let Some(ch) = node.get("children") else {
            continue;
        };
        let arr = ch
            .as_arr()
            .ok_or_else(|| glb_err(pack_path, format!("glb node {i} children must be an array")))?;
        if arr.len() > MAX_GLB_CHILDREN_PER_NODE {
            return Err(glb_err(
                pack_path,
                format!("glb node {i} children exceed {MAX_GLB_CHILDREN_PER_NODE}"),
            ));
        }
        for c in arr {
            let idx = json_u64(c)
                .and_then(|v| usize::try_from(v).ok())
                .ok_or_else(|| glb_err(pack_path, format!("glb node {i} child index")))?;
            if idx >= n {
                return Err(glb_err(pack_path, format!("glb node {i} child {idx} out of range")));
            }
            if idx == i {
                return Err(glb_err(pack_path, format!("glb node {i} is its own child")));
            }
            children[i].push(idx);
        }
    }
    let mut state = vec![0u8; n];
    let mut depth_at = vec![0u32; n];
    for i in 0..n {
        if state[i] == 0 {
            glb_dfs_nodes(i, &children, &mut state, &mut depth_at, 0, pack_path)?;
        }
    }
    Ok(())
}

fn glb_dfs_nodes(
    i: usize,
    children: &[Vec<usize>],
    state: &mut [u8],
    depth_at: &mut [u32],
    depth: u32,
    pack_path: &str,
) -> Result<(), PackImportError> {
    if depth as usize > MAX_GLB_NODE_DEPTH {
        return Err(glb_err(
            pack_path,
            format!("glb node depth exceeds {MAX_GLB_NODE_DEPTH}"),
        ));
    }
    match state[i] {
        1 => return Err(glb_err(pack_path, format!("glb node {i} cycle"))),
        2 => {
            if depth_at[i] <= depth {
                return Ok(());
            }
        }
        _ => {}
    }
    state[i] = 1;
    for &c in &children[i] {
        glb_dfs_nodes(c, children, state, depth_at, depth + 1, pack_path)?;
    }
    state[i] = 2;
    depth_at[i] = depth_at[i].max(depth);
    Ok(())
}

fn preflight_glb_meshes(value: &Value, pack_path: &str) -> Result<(), PackImportError> {
    let meshes = match value.get("meshes") {
        None | Some(Value::Null) => return Ok(()),
        Some(v) => v
            .as_arr()
            .ok_or_else(|| glb_err(pack_path, "glb meshes must be an array"))?,
    };
    if meshes.len() > MAX_GLB_MESHES {
        return Err(glb_err(pack_path, format!("glb meshes exceed {MAX_GLB_MESHES}")));
    }
    let accessor_n = value
        .get("accessors")
        .and_then(|v| v.as_arr())
        .map(|a| a.len())
        .unwrap_or(0);
    let mut prims = 0usize;
    for (mi, mesh) in meshes.iter().enumerate() {
        let Some(parr) = mesh.get("primitives") else {
            continue;
        };
        let arr = parr
            .as_arr()
            .ok_or_else(|| glb_err(pack_path, format!("glb mesh {mi} primitives")))?;
        prims = prims
            .checked_add(arr.len())
            .ok_or_else(|| glb_err(pack_path, "glb primitive count overflow"))?;
        if prims > MAX_GLB_PRIMITIVES {
            return Err(glb_err(
                pack_path,
                format!("glb primitives exceed {MAX_GLB_PRIMITIVES}"),
            ));
        }
        for prim in arr {
            if let Some(attrs) = prim.get("attributes") {
                if let Value::Obj(pairs) = attrs {
                    for (name, v) in pairs {
                        let idx = json_u64(v).and_then(|n| usize::try_from(n).ok()).ok_or_else(|| {
                            glb_err(pack_path, format!("glb primitive attribute {name}"))
                        })?;
                        if idx >= accessor_n {
                            return Err(glb_err(
                                pack_path,
                                format!("glb primitive attribute {name} accessor {idx}"),
                            ));
                        }
                    }
                }
            }
            if let Some(ii) = json_opt_u64(prim, "indices")? {
                if usize::try_from(ii).ok().filter(|i| *i < accessor_n).is_none() {
                    return Err(glb_err(pack_path, "glb primitive indices accessor"));
                }
            }
        }
    }
    Ok(())
}

trait BoundsExt {
    fn validate_pack(&self, pack_path: &str) -> Result<(), PackImportError>;
}

impl BoundsExt for Bounds {
    fn validate_pack(&self, pack_path: &str) -> Result<(), PackImportError> {
        for v in [
            self.min.x, self.min.y, self.min.z, self.max.x, self.max.y, self.max.z,
        ] {
            if !v.is_finite() {
                return Err(PackImportError::new(
                    PackImportErrorKind::Malformed,
                    format!("{pack_path}: non-finite bounds"),
                ));
            }
        }
        if self.min.x > self.max.x || self.min.y > self.max.y || self.min.z > self.max.z {
            return Err(PackImportError::new(
                PackImportErrorKind::Malformed,
                format!("{pack_path}: inverted bounds"),
            ));
        }
        Ok(())
    }
}

fn write_outputs(
    pack: &PackRoot,
    out_dir: &Path,
    built: &BuiltPack,
) -> Result<PackCompileReport, PackImportError> {
    let source_bytes = built.collection.to_canonical_bytes().map_err(|e| {
        PackImportError::new(
            PackImportErrorKind::Content,
            format!("source collection encode: {e}"),
        )
    })?;
    let manifest_bytes = built.manifest.to_canonical_bytes().map_err(|e| {
        PackImportError::new(
            PackImportErrorKind::Content,
            format!("import manifest encode: {e}"),
        )
    })?;
    let source_digest = built.collection.digest().map_err(|e| {
        PackImportError::new(
            PackImportErrorKind::Content,
            format!("source collection digest: {e}"),
        )
    })?;
    if sha256(&source_bytes) != *source_digest.as_bytes() {
        return Err(PackImportError::new(
            PackImportErrorKind::Content,
            "source collection digest drifted",
        ));
    }
    let import_revision = built.manifest.revision().map_err(|e| {
        PackImportError::new(PackImportErrorKind::Content, format!("import revision: {e}"))
    })?;
    let plan = upload_plan_json(built, &source_digest, &import_revision);
    let dests = [
        (SOURCE_COLLECTION_FILE, source_bytes.as_slice()),
        (IMPORT_MANIFEST_FILE, manifest_bytes.as_slice()),
        (UPLOAD_PLAN_FILE, plan.as_bytes()),
    ];
    assert_pack_identity(pack)?;
    assert_out_outside_pack(pack, out_dir)?;
    match classify_out(out_dir)? {
        OutState::Absent => commit_new_bundle(pack, out_dir, &dests)?,
        OutState::Exact => {
            assert_existing_out_outside_pack(pack, out_dir)?;
            if !existing_bundle_matches(out_dir, &dests)? {
                return Err(PackImportError::new(
                    PackImportErrorKind::Io,
                    format!(
                        "--out {} exists but bytes diverge; choose another path",
                        out_dir.display()
                    ),
                ));
            }
        }
        OutState::Refuse(kind, msg) => return Err(PackImportError::new(kind, msg)),
    }
    Ok(PackCompileReport {
        source_digest,
        import_revision,
        assets: built.manifest.assets.len(),
        blobs: built.blobs.len(),
        source_path: out_dir.join(SOURCE_COLLECTION_FILE),
        manifest_path: out_dir.join(IMPORT_MANIFEST_FILE),
        plan_path: out_dir.join(UPLOAD_PLAN_FILE),
        skipped_models: built.skipped.clone(),
    })
}

enum OutState {
    Absent,
    Exact,
    Refuse(PackImportErrorKind, String),
}

fn classify_out(out_dir: &Path) -> Result<OutState, PackImportError> {
    let meta = match fs::symlink_metadata(out_dir) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(OutState::Absent),
        Err(e) => {
            return Err(PackImportError::new(
                PackImportErrorKind::Io,
                format!("stat --out {}: {e}", out_dir.display()),
            ))
        }
    };
    if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
        return Ok(OutState::Refuse(
            PackImportErrorKind::Special,
            format!(
                "--out {} is not a regular directory; choose another path",
                out_dir.display()
            ),
        ));
    }
    let mut seen = BTreeSet::new();
    let reader = fs::read_dir(out_dir).map_err(|e| {
        PackImportError::new(
            PackImportErrorKind::Io,
            format!("read --out {}: {e}", out_dir.display()),
        )
    })?;
    for entry in reader {
        let entry = entry.map_err(|e| {
            PackImportError::new(
                PackImportErrorKind::Io,
                format!("read --out {}: {e}", out_dir.display()),
            )
        })?;
        let name = entry.file_name();
        let name = name.to_string_lossy().into_owned();
        let child = out_dir.join(&name);
        let child_meta = fs::symlink_metadata(&child).map_err(|e| {
            PackImportError::new(PackImportErrorKind::Io, format!("stat {}: {e}", child.display()))
        })?;
        if child_meta.file_type().is_symlink() || !child_meta.file_type().is_file() {
            return Ok(OutState::Refuse(
                PackImportErrorKind::Special,
                format!(
                    "--out {} contains a non-regular leaf {name}; choose another path",
                    out_dir.display()
                ),
            ));
        }
        if name != SOURCE_COLLECTION_FILE
            && name != IMPORT_MANIFEST_FILE
            && name != UPLOAD_PLAN_FILE
        {
            return Ok(OutState::Refuse(
                PackImportErrorKind::Io,
                format!(
                    "--out {} has extra content {name}; choose another path",
                    out_dir.display()
                ),
            ));
        }
        seen.insert(name);
    }
    if seen.len() != 3
        || !seen.contains(SOURCE_COLLECTION_FILE)
        || !seen.contains(IMPORT_MANIFEST_FILE)
        || !seen.contains(UPLOAD_PLAN_FILE)
    {
        return Ok(OutState::Refuse(
            PackImportErrorKind::Io,
            format!(
                "--out {} is not a complete 3-file bundle; choose another path",
                out_dir.display()
            ),
        ));
    }
    Ok(OutState::Exact)
}

fn existing_bundle_matches(
    out_dir: &Path,
    dests: &[(&str, &[u8]); 3],
) -> Result<bool, PackImportError> {
    for (name, want) in dests {
        let path = out_dir.join(name);
        let mut file = open_path_regular_nofollow(&path, name)?;
        let meta = file.metadata().map_err(|e| {
            PackImportError::new(PackImportErrorKind::Io, format!("fstat existing {name}: {e}"))
        })?;
        if meta.len() != want.len() as u64 {
            return Ok(false);
        }
        let got = read_exact_capped(&mut file, meta.len(), MAX_DOCUMENT_BYTES as u64, name)?;
        if got.as_slice() != *want {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn assert_pack_identity(_pack: &PackRoot) -> Result<(), PackImportError> {
    Err(pack_os_unsupported())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn assert_pack_identity(pack: &PackRoot) -> Result<(), PackImportError> {
    #[cfg(unix)]
    {
        let meta = pack.dir.metadata().map_err(|e| {
            PackImportError::new(PackImportErrorKind::Io, format!("fstat pack root: {e}"))
        })?;
        if meta.dev() != pack.dev || meta.ino() != pack.ino {
            return Err(PackImportError::new(
                PackImportErrorKind::Changed,
                "pack root identity changed before publish",
            ));
        }
    }
    let _ = pack;
    Ok(())
}

fn assert_out_outside_pack(pack: &PackRoot, out_dir: &Path) -> Result<(), PackImportError> {
    let projected = project_out_path(out_dir)?;
    refuse_if_inside_pack(&pack.path, &projected)?;
    Ok(())
}

fn assert_existing_out_outside_pack(
    pack: &PackRoot,
    out_dir: &Path,
) -> Result<(), PackImportError> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let dir = unix::open_dir_path(out_dir)?;
        unix::assert_fd_outside_pack(&dir, pack, "existing --out")?;
        return Ok(());
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (pack, out_dir);
        Err(pack_os_unsupported())
    }
}

fn commit_new_bundle(
    pack: &PackRoot,
    out_dir: &Path,
    dests: &[(&str, &[u8]); 3],
) -> Result<(), PackImportError> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        return unix::publish_bundle(pack, out_dir, dests);
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (pack, out_dir, dests);
        Err(pack_os_unsupported())
    }
}

fn cleanup_staging(staging: &Path) {
    let Some(name) = staging.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    if !name.starts_with(STAGING_PREFIX) {
        return;
    }
    let _ = fs::remove_dir_all(staging);
}

fn upload_plan_json(
    built: &BuiltPack,
    source_digest: &SourceCollectionId,
    import_revision: &makepad_asset_data::ImportRevisionId,
) -> String {
    let namespace = built.collection.id.clone();
    let mut blob_rows = Vec::with_capacity(built.blobs.len());
    for b in &built.blobs {
        blob_rows.push(obj(vec![
            ("path", s(b.pack_path.clone())),
            ("local_path", s(b.local_rel.clone())),
            ("path_kind", s("pack-root-relative")),
            ("blob", s(b.blob.to_string())),
            ("byte_len", Value::Int(b.byte_len as i64)),
            ("media", s(b.media.name())),
            ("role", s(role_name(b.role))),
            ("namespace", s(namespace.clone())),
            ("reverify_digest", Value::Bool(true)),
        ]));
    }
    let mut asset_rows = Vec::with_capacity(built.manifest.assets.len());
    for a in &built.manifest.assets {
        let alias = built
            .manifest
            .alias_for(&a.key)
            .map(|al| al.as_str().to_string())
            .unwrap_or_default();
        let asset_id = built.manifest.asset_id_for(&a.key);
        asset_rows.push(obj(vec![
            ("key", s(a.key.as_str())),
            ("alias", s(alias)),
            ("asset_id", s(asset_id.to_string())),
            ("kind", s(kind_name(a.kind))),
        ]));
    }
    obj(vec![
        ("schema", s(PLAN_SCHEMA)),
        ("namespace", s(namespace.clone())),
        (
            "client",
            obj(vec![
                (
                    "register_source_collection",
                    s("AssetClient::register_source_collection"),
                ),
                ("upload_blob", s("AssetClient::upload_blob")),
                ("run_import", s("AssetClient::run_import")),
            ]),
        ),
        (
            "steps",
            Value::Arr(vec![
                obj(vec![
                    ("op", s("register_source_collection")),
                    ("file", s(SOURCE_COLLECTION_FILE)),
                    ("expect_digest", s(source_digest.to_string())),
                    ("expect_source_id", s(namespace.clone())),
                    ("namespace", s(namespace.clone())),
                ]),
                obj(vec![
                    ("op", s("upload_blob")),
                    ("namespace", s(namespace.clone())),
                    ("blobs", s("blobs")),
                    ("path_kind", s("pack-root-relative")),
                    (
                        "note",
                        s("read each pack-root-relative local_path; sha256 must equal blob; upload_blob(namespace, bytes)"),
                    ),
                ]),
                obj(vec![
                    ("op", s("run_import")),
                    ("file", s(IMPORT_MANIFEST_FILE)),
                    ("expect_revision", s(import_revision.to_string())),
                    ("namespace", s(namespace.clone())),
                ]),
            ]),
        ),
        (
            "source_collection",
            obj(vec![
                ("id", s(built.collection.id.clone())),
                ("title", s(built.collection.title.clone())),
                ("digest", s(source_digest.to_string())),
                ("file", s(SOURCE_COLLECTION_FILE)),
            ]),
        ),
        (
            "import_manifest",
            obj(vec![
                ("revision", s(import_revision.to_string())),
                ("source_id", s(built.manifest.source_id.clone())),
                ("pack_name", s(built.manifest.pack_name.clone())),
                ("pack_version", s(built.manifest.pack_version.clone())),
                (
                    "policy_version",
                    Value::Int(built.manifest.policy_version as i64),
                ),
                ("file", s(IMPORT_MANIFEST_FILE)),
            ]),
        ),
        (
            "rights",
            obj(vec![
                ("license", s(built.manifest.rights.license.clone())),
                (
                    "license_revision",
                    s(built.manifest.rights.license_revision.clone()),
                ),
                (
                    "terms_digest",
                    match built.manifest.rights.terms_digest {
                        Some(d) => s(hex32(&d)),
                        None => Value::Null,
                    },
                ),
                ("terms_url", s(built.manifest.rights.terms_url.clone())),
                ("credits", s(built.manifest.rights.credits.clone())),
                ("source", s(built.manifest.rights.source.clone())),
                (
                    "source_archive",
                    match built.manifest.rights.source_archive {
                        Some(d) => s(hex32(&d)),
                        None => Value::Null,
                    },
                ),
                (
                    "redistribution",
                    s(redistribution_name(built.manifest.rights.redistribution)),
                ),
                (
                    "derivatives",
                    s(derivatives_name(built.manifest.rights.derivatives)),
                ),
            ]),
        ),
        ("uploader", s(UPLOADER_REVERIFY)),
        ("blobs", Value::Arr(blob_rows)),
        ("assets", Value::Arr(asset_rows)),
    ])
    .to_json()
}

fn hex32(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn role_name(role: FileRole) -> &'static str {
    match role {
        FileRole::RenderGlb => "render_glb",
        FileRole::Lod1Glb => "lod1_glb",
        FileRole::Lod2Glb => "lod2_glb",
        FileRole::Collider => "collider",
        FileRole::AoMesh => "ao_mesh",
        FileRole::ShadowSdf => "shadow_sdf",
        FileRole::Albedo => "albedo",
        FileRole::Normal => "normal",
        FileRole::Orm => "orm",
        FileRole::Texture => "texture",
        FileRole::PreviewFront => "preview_front",
        FileRole::PreviewSide => "preview_side",
        FileRole::Turntable => "turntable",
        FileRole::Audio => "audio",
        FileRole::Video => "video",
        FileRole::Source => "source",
        FileRole::Depth => "depth",
        FileRole::Splat => "splat",
        FileRole::AoTexture => "ao_texture",
        FileRole::StemDrums => "stem_drums",
        FileRole::StemBass => "stem_bass",
        FileRole::StemVocals => "stem_vocals",
        FileRole::StemOther => "stem_other",
        FileRole::Lyrics => "lyrics",
        FileRole::DjAnalysis => "dj_analysis",
        FileRole::DjLoopSplat => "dj_loop_splat",
        FileRole::MediaTimeline => "media_timeline",
        FileRole::MediaTimelinePage => "media_timeline_page",
        FileRole::CaptionsSrt => "captions_srt",
        FileRole::CaptionsVtt => "captions_vtt",
        FileRole::Image => "image",
    }
}

fn kind_name(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Mesh => "mesh",
        AssetKind::Character => "character",
        AssetKind::Weapon => "weapon",
        AssetKind::Vehicle => "vehicle",
        AssetKind::Prop => "prop",
        AssetKind::Texture => "texture",
        AssetKind::Material => "material",
        AssetKind::Audio => "audio",
        AssetKind::Video => "video",
        AssetKind::Skybox => "skybox",
        AssetKind::World => "world",
        AssetKind::Prefab => "prefab",
        AssetKind::Billboard => "billboard",
        AssetKind::Game => "game",
        AssetKind::VjEffect => "vjeffect",
        AssetKind::Data => "data",
        AssetKind::ModelProgram => "model-program",
        AssetKind::Cut => "cut",
        AssetKind::AppRun => "app-run",
        AssetKind::Flow => "flow",
        AssetKind::Motion => "motion",
        AssetKind::Storyboard => "storyboard",
        AssetKind::Score => "score",
        AssetKind::Deck => "deck",
        AssetKind::Tree => "tree",
    }
}

fn redistribution_name(p: Redistribution) -> &'static str {
    match p {
        Redistribution::Allowed => "allowed",
        Redistribution::AttributionRequired => "attribution-required",
        Redistribution::Forbidden => "forbidden",
        Redistribution::LanLocal => "lan-local",
    }
}

fn derivatives_name(p: DerivativePolicy) -> &'static str {
    match p {
        DerivativePolicy::Allowed => "allowed",
        DerivativePolicy::AttributionRequired => "attribution-required",
        DerivativePolicy::Forbidden => "forbidden",
        DerivativePolicy::LocalPreview => "local-preview-only",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thumbs::encode_jpeg_bgra;
    use makepad_asset_data::{ImportManifest, SourceCollection};
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn ui_manifest_extensions_are_text_source_assets() {
        for (extension, kind) in [
            ("ui", MediaKind::UiSource),
            ("font", MediaKind::FontSource),
        ] {
            assert_eq!(MediaKind::from_ext(extension), Some(kind));
            assert_eq!(kind.media_type(), MediaType::Text);
            assert!(!kind.is_sidecar());
        }
    }

    /// The `.place` placements sidecar (world-place 1 text) rides the same
    /// sidecar lane as `.spawn`: recognized by extension, attached to the
    /// same-stem GLB, published as a Source-role TEXT blob (a typed role
    /// waits for the next schema bump — new canon_enum tags hard-error
    /// deployed clients). Consumers pick it out of the Source files by its
    /// self-identifying first line.
    #[test]
    fn place_sidecar_recognized_and_text_typed() {
        assert_eq!(MediaKind::from_ext("place"), Some(MediaKind::Place));
        assert!(MediaKind::Place.is_sidecar());
        assert_eq!(MediaKind::Place.media_type(), MediaType::Text);
        assert_eq!(MediaKind::Place.name(), "place");
    }

    fn test_root(name: &str) -> PathBuf {
        let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "mp_pack_import_{}_{}_{}",
            std::process::id(),
            n,
            name
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn test_bundle(name: &str) -> PathBuf {
        test_root(name).join("bundle")
    }

    fn terms_hex() -> String {
        kenney_terms_digest_hex()
    }

    fn licensed_spec() -> PackSourceSpec {
        kenney_spec("space-kit").expect("space-kit is a catalogued Kenney pack")
    }

    fn png_header(w: u32, h: u32) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&w.to_be_bytes());
        png.extend_from_slice(&h.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        png
    }

    fn valid_png(w: u32, h: u32) -> Vec<u8> {
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        let bpp = 4u32;
        let row = 1 + w * bpp;
        let raw_len = (row as usize)
            .checked_mul(h as usize)
            .expect("png dims");
        // A real thumbnail is not one flat colour — the compiler refuses
        // placeholders, so the fixture must look like a picture.
        let mut raw = vec![0u8; raw_len];
        for y in 0..h as usize {
            let row = y * row as usize;
            for x in 0..w as usize {
                let p = row + 1 + x * 4;
                raw[p] = (x * 7) as u8;
                raw[p + 1] = (y * 5) as u8;
                raw[p + 2] = ((x + y) * 3) as u8;
                raw[p + 3] = 255;
            }
        }
        let mut zlib = vec![0x78, 0x01];
        let mut off = 0usize;
        while off < raw.len() {
            let take = (raw.len() - off).min(65535);
            let last = off + take == raw.len();
            zlib.push(if last { 0x01 } else { 0x00 });
            let n = take as u16;
            zlib.extend_from_slice(&n.to_le_bytes());
            zlib.extend_from_slice(&(!n).to_le_bytes());
            zlib.extend_from_slice(&raw[off..off + take]);
            off += take;
        }
        let mut s1 = 1u32;
        let mut s2 = 0u32;
        for &b in &raw {
            s1 = (s1 + b as u32) % 65521;
            s2 = (s2 + s1) % 65521;
        }
        zlib.extend_from_slice(&((s2 << 16) | s1).to_be_bytes());
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        push_png_chunk(&mut out, b"IHDR", &ihdr);
        push_png_chunk(&mut out, b"IDAT", &zlib);
        push_png_chunk(&mut out, b"IEND", &[]);
        out
    }

    fn push_png_chunk(out: &mut Vec<u8>, typ: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(typ);
        out.extend_from_slice(data);
        let mut crc_src = Vec::with_capacity(4 + data.len());
        crc_src.extend_from_slice(typ);
        crc_src.extend_from_slice(data);
        out.extend_from_slice(&crate::png::png_crc(&crc_src).to_be_bytes());
    }

    fn encode_test_mp4(dir: &Path) -> Vec<u8> {
        use makepad_platform::video_file::{VideoFileCodec, VideoFileEncoder, VideoFileEncoderOptions};
        let path = dir.join("probe.mp4");
        let (w, h) = (128u32, 64u32);
        let mut encoder = VideoFileEncoder::new(
            path.to_str().unwrap(),
            VideoFileEncoderOptions {
                codec: VideoFileCodec::H264,
                width: w,
                height: h,
                fps_num: 24,
                fps_den: 1,
                video_bitrate_bps: 4_000_000,
                audio: None,
                ..Default::default()
            },
        )
        .expect("encoder");
        let rgb = vec![200u8; (w * h * 3) as usize];
        for _ in 0..12 {
            encoder.push_frame_rgb8(&rgb, None).expect("frame");
        }
        encoder.finish().expect("finish");
        fs::read(&path).expect("read mp4")
    }

    fn shared_mp4() -> &'static [u8] {
        static MP4: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
        MP4.get_or_init(|| encode_test_mp4(&test_root("shared_mp4")))
    }

    fn jpeg_solid(w: usize, h: usize) -> Vec<u8> {
        let bgra = vec![0xff33_66aa_u32; w * h];
        encode_jpeg_bgra(&bgra, w, h).unwrap()
    }

    fn wav_pcm16(frames: &[(i16, i16)], rate: u32) -> Vec<u8> {
        let mut data = Vec::new();
        for (l, r) in frames {
            data.extend_from_slice(&l.to_le_bytes());
            data.extend_from_slice(&r.to_le_bytes());
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 4).to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    fn tiny_mp4(duration_ms: u32) -> Vec<u8> {
        // ftyp + moov/mvhd v0, timescale 1000.
        let mut mvhd = vec![0u8; 100];
        mvhd[0] = 0;
        mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
        mvhd[16..20].copy_from_slice(&duration_ms.to_be_bytes());
        let mut moov = Vec::new();
        write_box(&mut moov, b"mvhd", &mvhd);
        let mut out = Vec::new();
        write_box(&mut out, b"ftyp", b"isom");
        write_box(&mut out, b"moov", &moov);
        out
    }

    fn write_box(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
        let size = 8 + body.len() as u32;
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
    }

    fn tiny_glb() -> Vec<u8> {
        glb_with_positions([0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0])
    }

    /// One triangle spanning chosen extents, so a pack's calibration is
    /// measurable from its published bounds.
    fn glb_with_positions(positions: [f32; 9]) -> Vec<u8> {
        let mut bin: Vec<u8> = Vec::new();
        for f in positions {
            bin.extend_from_slice(&f.to_le_bytes());
        }
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},
            "nodes":[{{"mesh":0}}],
            "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}}}}]}}],
            "accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}}],
            "bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":{}}}],
            "buffers":[{{"byteLength":{}}}]}}"#,
            bin.len(),
            bin.len()
        );
        let mut json_bytes = json.into_bytes();
        while json_bytes.len() % 4 != 0 {
            json_bytes.push(b' ');
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(b"JSON");
        out.extend_from_slice(&json_bytes);
        out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(&bin);
        out
    }

    fn write_mixed_pack(root: &Path, mp4: &[u8]) {
        write_mixed_pack_in_order(root, mp4, false);
    }

    fn write_mixed_pack_reversed(root: &Path, mp4: &[u8]) {
        write_mixed_pack_in_order(root, mp4, true);
    }

    fn write_mixed_pack_in_order(root: &Path, mp4: &[u8], reversed: bool) {
        fs::create_dir_all(root.join("textures")).unwrap();
        fs::create_dir_all(root.join("audio")).unwrap();
        fs::create_dir_all(root.join("video")).unwrap();
        fs::create_dir_all(root.join("models")).unwrap();
        let frames: Vec<(i16, i16)> = (0..2_400).map(|i| (i as i16, -(i as i16))).collect();
        let mut files: Vec<(&str, Vec<u8>)> = vec![
            ("textures/hull.png", valid_png(512, 256)),
            ("textures/detail.jpg", jpeg_solid(64, 48)),
            ("audio/beep.wav", wav_pcm16(&frames, 24_000)),
            ("video/clip.mp4", mp4.to_vec()),
            ("models/box.glb", tiny_glb()),
            ("models/box.png", valid_png(512, 512)),
            ("License.txt", b"CC-BY-4.0 Kenney".to_vec()),
            ("README.md", b"docs".to_vec()),
        ];
        if reversed {
            files.reverse();
        }
        for (rel, bytes) in files {
            fs::write(root.join(rel), bytes).unwrap();
        }
    }

    fn discovered(local_rel: &str, pack_path: &str, key: &str) -> DiscoveredFile {
        DiscoveredFile {
            local_rel: local_rel.into(),
            pack_path: pack_path.into(),
            key: key.into(),
            kind: MediaKind::Png,
            snapshot: FileIdentity {
                len: 1,
                modified: None,
                is_file: true,
                #[cfg(unix)]
                dev: 0,
                #[cfg(unix)]
                ino: 0,
            },
        }
    }

    fn compile(pack: &Path, out: &Path, spec: PackSourceSpec) -> PackCompileReport {
        compile_pack(pack, out, spec, None, false).expect("compile")
    }

    #[test]
    fn classify_canonicalizes_kenney_style_paths() {
        let (path, key, kind) = classify_rel("Models/GLTF format/WatchTower.GLB").unwrap();
        assert_eq!(path, "models/gltf-format/watchtower.glb");
        assert_eq!(key, "models/gltf-format/watchtower");
        assert_eq!(kind, MediaKind::Glb);
        assert!(classify_rel("../etc/passwd.png").is_err());
        assert!(classify_rel("/abs/x.png").is_err());
        assert!(classify_rel("foo/../../x.png").is_err());
        assert!(classify_rel("foo\\..\\bar.png").unwrap_err().kind == PackImportErrorKind::Traversal);
    }

    #[test]
    fn user_owned_local_rights_are_lan_local_and_importable() {
        // The classic packs' declaration: the user's own game data is served
        // on the user's LAN (this catalog) and nothing leaves it.
        assert_eq!(parse_redistribution("user-owned-local").unwrap(), Redistribution::LanLocal);
        assert_eq!(parse_redistribution("lan-local").unwrap(), Redistribution::LanLocal);
        assert_eq!(
            parse_derivatives("local-preview-only").unwrap(),
            DerivativePolicy::LocalPreview
        );
        assert_eq!(redistribution_name(Redistribution::LanLocal), "lan-local");
        assert_eq!(derivatives_name(DerivativePolicy::LocalPreview), "local-preview-only");
    }

    #[test]
    fn rights_fail_closed_never_defaults_cc0() {
        let mut spec = licensed_spec();
        spec.license = None;
        let err = spec.clone().resolve().unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Rights);
        assert!(err.to_string().contains("license"));

        spec = licensed_spec();
        spec.terms_digest = None;
        assert_eq!(spec.resolve().unwrap_err().kind, PackImportErrorKind::Rights);

        spec = licensed_spec();
        spec.terms_url = None;
        assert_eq!(spec.resolve().unwrap_err().kind, PackImportErrorKind::Rights);

        spec = licensed_spec();
        spec.credits = Some(String::new());
        assert_eq!(spec.resolve().unwrap_err().kind, PackImportErrorKind::Rights);

        spec = licensed_spec();
        spec.redistribution = Some("forbidden".into());
        let err = spec.resolve().unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Rights);
        assert!(err.to_string().contains("forbidden"));

        spec = licensed_spec();
        spec.redistribution = Some("allowed".into());
        spec.derivatives = Some("attribution-required".into());
        spec.credits = Some(String::new());
        assert_eq!(spec.resolve().unwrap_err().kind, PackImportErrorKind::Rights);

        // An explicit CC-BY grant resolves; nothing fills in CC0.
        let resolved = licensed_spec().resolve().unwrap();
        assert_eq!(resolved.rights.license, "CC-BY-4.0");
        assert_ne!(resolved.rights.license, "CC0-1.0");
        assert!(resolved.rights.terms_digest.is_some());

        // compile_pack itself refuses a complete pack with incomplete rights.
        let pack = test_root("rights_pack");
        let out = test_bundle("rights_out");
        fs::write(pack.join("panel.png"), valid_png(32, 32)).unwrap();
        let mut missing = licensed_spec();
        missing.license = None;
        let err = compile_pack(&pack, &out, missing, None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Rights);
        assert!(err.to_string().contains("license"));
        assert!(!err.to_string().to_ascii_lowercase().contains("cc0-1.0"));

        missing = licensed_spec();
        missing.terms_digest = Some(String::new());
        assert_eq!(
            compile_pack(&pack, &out, missing, None, false)
                .unwrap_err()
                .kind,
            PackImportErrorKind::Rights
        );
        missing = licensed_spec();
        missing.terms_url = None;
        assert_eq!(
            compile_pack(&pack, &out, missing, None, false)
                .unwrap_err()
                .kind,
            PackImportErrorKind::Rights
        );
    }

    /// Unreadable derived sidecars never block a pack: the GLB compiles as
    /// a plain mesh (the shadow SDF is opaque bytes and still attaches).
    #[test]
    fn unreadable_sidecars_are_left_behind_so_kenney_source_compiles() {
        let pack = test_root("sidecars");
        let out = test_bundle("sidecars_out");
        fs::write(pack.join("crate.glb"), tiny_glb()).unwrap();
        fs::write(pack.join("crate.png"), valid_png(512, 512)).unwrap();
        fs::write(pack.join("crate.aomesh"), b"not-a-source").unwrap();
        fs::write(pack.join("crate.shadowsdf"), b"not-a-source").unwrap();
        fs::write(pack.join("crate.ao.png"), valid_png(64, 64)).unwrap();
        let report = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap();
        assert_eq!(report.assets, 1);
        let manifest = ImportManifest::from_canonical_bytes(&fs::read(&report.manifest_path).unwrap())
            .unwrap();
        let roles: Vec<FileRole> = manifest.assets[0].files.iter().map(|f| f.file.role).collect();
        assert_eq!(roles, [FileRole::RenderGlb, FileRole::ShadowSdf], "{roles:?}");
    }

    /// A real AO bake beside its GLB publishes as explicit derived roles on
    /// the SAME asset (aomesh + atlas together, shadow SDF alone), so a game
    /// streaming the mesh gets the bake from one manifest.
    #[test]
    fn baked_sidecars_attach_to_their_glb_as_derived_roles() {
        let pack = test_root("sidecars_attach");
        let out = test_bundle("sidecars_attach_out");
        let glb = tiny_glb();
        fs::write(pack.join("crate.glb"), &glb).unwrap();
        fs::write(pack.join("crate.png"), valid_png(512, 512)).unwrap();
        let aomesh = StaticModel::parse_glb(&glb).expect("tiny glb parses").to_aomesh();
        assert!(StaticModel::from_aomesh(&aomesh).is_some());
        fs::write(pack.join("crate.aomesh"), &aomesh).unwrap();
        fs::write(pack.join("crate.ao.png"), valid_png(64, 64)).unwrap();
        fs::write(pack.join("crate.shadowsdf"), b"sdf-bytes-opaque").unwrap();
        // An orphan sidecar (no GLB) is neither an asset nor an error.
        fs::write(pack.join("ghost.aomesh"), &aomesh).unwrap();
        let report = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap();
        assert_eq!(report.assets, 1);
        let manifest = ImportManifest::from_canonical_bytes(&fs::read(&report.manifest_path).unwrap())
            .unwrap();
        let asset = &manifest.assets[0];
        let mut roles: Vec<FileRole> = asset.files.iter().map(|f| f.file.role).collect();
        roles.sort();
        assert_eq!(
            roles,
            [FileRole::RenderGlb, FileRole::AoMesh, FileRole::ShadowSdf, FileRole::AoTexture],
            "{roles:?}"
        );
        let ao_png = asset.files.iter().find(|f| f.file.role == FileRole::AoTexture).unwrap();
        assert_eq!(ao_png.file.media, MediaType::Png);
        assert_eq!(ao_png.file.dims.map(|d| (d.width, d.height)), Some((64, 64)));
        let sum: u64 = asset.files.iter().map(|f| f.file.byte_len).sum::<u64>()
            + asset.thumbnail.as_ref().map(|t| t.meta.byte_len).unwrap_or(0);
        assert_eq!(asset.metrics.total_bytes, sum);
        assert_eq!(report.blobs, 5, "glb + thumb + 3 sidecars");
    }

    #[test]
    fn kenney_catalog_is_cc_by_and_refuses_unknown_packs() {
        let space = kenney_spec("space-kit").unwrap();
        assert_eq!(space.source_id.as_deref(), Some(KENNEY_SOURCE_ID));
        assert_eq!(space.license.as_deref(), Some("CC-BY-4.0"));
        assert_ne!(space.license.as_deref(), Some("CC0-1.0"));
        assert_eq!(space.credits.as_deref(), Some(KENNEY_CREDITS));
        assert_eq!(space.source.as_deref(), Some(KENNEY_ASSETS_HOME));
        assert_eq!(space.redistribution.as_deref(), Some("attribution-required"));
        let ui = kenney_spec("ui-pack").unwrap();
        assert_eq!(ui.pack_name.as_deref(), Some("ui-pack"));
        // ONE registered collection for every kit: the rights (and so the
        // collection digest) must not vary with the pack. The per-kit page
        // is a separate lookup.
        assert_eq!(ui.source, space.source);
        assert_eq!(kenney_page("ui-pack"), "https://kenney.nl/assets/ui-pack");
        let err = kenney_spec("Not A Pack").unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Config);
        let extra = kenney_spec("castle-kit").unwrap();
        assert_eq!(extra.license.as_deref(), Some("CC-BY-4.0"));
        assert_eq!(extra.source, space.source);
        assert_eq!(kenney_page("castle-kit"), "https://kenney.nl/assets/castle-kit");
        let (a, b) = (
            resolve_kenney_collection("space-kit"),
            resolve_kenney_collection("castle-kit"),
        );
        assert_eq!(a.digest().unwrap(), b.digest().unwrap(), "collection digest drifts per kit");
    }

    fn resolve_kenney_collection(pack: &str) -> SourceCollection {
        let source = kenney_spec(pack).unwrap().resolve().expect("resolve");
        SourceCollection {
            id: source.source_id.clone(),
            title: source.source_title.clone(),
            origin: SourceOrigin::Upload,
            terms: source.rights.clone(),
        }
    }

    #[test]
    fn source_config_is_source_only_and_overlays_cli() {
        let err = parse_source_config(br#"{"source_id":"kenney","files":[]}"#).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Config);
        assert!(err.to_string().contains("source-only"));

        let err = parse_source_config(br#"{"source_id":"kenney","mystery":"x"}"#).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Config);

        let cfg = parse_source_config(
            format!(
                r#"{{"source_id":"kenney","source_title":"Kenney game assets","pack_name":"space-kit","pack_version":"1.0","license":"CC-BY-4.0","terms_digest":"{}","terms_url":"https://creativecommons.org/licenses/by/4.0/","credits":"Kenney (kenney.nl)","source":"https://kenney.nl/assets/space-kit","redistribution":"attribution-required","derivatives":"allowed"}}"#,
                terms_hex()
            )
            .as_bytes(),
        )
        .unwrap();
        let mut merged = cfg;
        merged.overlay(PackSourceSpec {
            pack_version: Some("1.1".into()),
            ..PackSourceSpec::default()
        });
        let resolved = merged.resolve().unwrap();
        assert_eq!(resolved.pack_version, "1.1");
        assert_eq!(resolved.source_id, "kenney");
    }

    #[test]
    fn empty_pack_and_docs_only_refuse() {
        let pack = test_root("empty");
        let out = test_bundle("empty_out");
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Empty);

        fs::write(pack.join("README.md"), b"docs").unwrap();
        fs::write(pack.join("License.txt"), b"terms").unwrap();
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Empty);
    }

    #[test]
    fn unsupported_and_malformed_payloads_refuse() {
        let pack = test_root("bad");
        let out = test_bundle("bad_out");
        fs::write(pack.join("model.obj"), b"v 0 0 0").unwrap();
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Unsupported);

        let pack = test_root("badpng");
        fs::write(pack.join("x.png"), b"not a png").unwrap();
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed);

        let pack = test_root("badwav");
        fs::write(pack.join("x.wav"), b"RIFF????WAVE").unwrap();
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed);

        let pack = test_root("badglb");
        fs::write(pack.join("preview.png"), valid_png(512, 512)).unwrap();
        fs::write(pack.join("x.glb"), b"glTFnotreally").unwrap();
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed);

        let pack = test_root("badmp4");
        fs::write(pack.join("x.mp4"), b"not an mp4").unwrap();
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed);

        let pack = test_root("badjpg");
        fs::write(pack.join("x.jpg"), b"\xff\xd8junk").unwrap();
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn unix_nofollow_and_directory_flags_enforce_kernel_semantics() {
        use std::os::unix::fs::symlink;

        let dir = test_root("unix_flags");
        let file = dir.join("file");
        let sub = dir.join("sub");
        let file_link = dir.join("file_link");
        let dir_link = dir.join("dir_link");
        let bytes = b"pack";
        fs::write(&file, bytes).unwrap();
        fs::create_dir(&sub).unwrap();
        symlink("file", &file_link).unwrap();
        symlink("sub", &dir_link).unwrap();

        let mut nofollow = OpenOptions::new();
        nofollow.read(true).custom_flags(unix::O_NOFOLLOW);
        let err = nofollow.open(&file_link).unwrap_err();
        #[cfg(target_os = "linux")]
        assert_eq!(err.raw_os_error(), Some(40), "expected ELOOP");
        #[cfg(target_os = "macos")]
        assert_eq!(err.raw_os_error(), Some(62), "expected ELOOP");
        let mut contents = Vec::new();
        nofollow.open(&file).unwrap().read_to_end(&mut contents).unwrap();
        assert_eq!(contents.as_slice(), bytes);

        let mut directory = OpenOptions::new();
        directory.read(true).custom_flags(unix::O_DIRECTORY);
        let err = directory.open(&file).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(20), "expected ENOTDIR");
        directory.open(&sub).unwrap();

        unix::open_dir_path(&sub).unwrap();
        assert_eq!(unix::open_dir_path(&dir_link).unwrap_err().kind, PackImportErrorKind::Io);
        assert_eq!(unix::open_dir_path(&file).unwrap_err().kind, PackImportErrorKind::Io);

        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn unix_openat_entry_and_nofollow_classify_all_entry_kinds() {
        use std::os::unix::{fs::symlink, net::UnixListener};

        fn assert_error<T>(result: Result<T, PackImportError>, kind: PackImportErrorKind, name: &str) {
            let err = result.err().unwrap_or_else(|| panic!("unexpected success for {name:?}"));
            assert_eq!(err.kind, kind, "{name:?}: {err}");
        }

        let dir = test_root("unix_entry");
        fs::write(dir.join("reg.bin"), b"hello").unwrap();
        fs::create_dir(dir.join("sub")).unwrap();
        symlink("reg.bin", dir.join("lnk_file")).unwrap();
        symlink("sub", dir.join("lnk_dir")).unwrap();
        symlink("missing", dir.join("lnk_bad")).unwrap();
        assert!(std::process::Command::new("mkfifo")
            .arg(dir.join("pipe.fifo"))
            .status()
            .expect("mkfifo")
            .success());
        let socket = UnixListener::bind(dir.join("s.sock")).unwrap();
        let d = unix::open_dir_path(&dir).unwrap();

        match unix::openat_entry_nofollow(&d, "reg.bin", "reg.bin").unwrap() {
            unix::AtEntry::Regular(mut file, meta) => {
                assert!(meta.file_type().is_file());
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes).unwrap();
                assert_eq!(bytes.as_slice(), b"hello");
            }
            unix::AtEntry::Directory(_) => panic!("reg.bin classified as a directory"),
        }
        match unix::openat_entry_nofollow(&d, "sub", "sub").unwrap() {
            unix::AtEntry::Directory(sub) => assert!(sub.metadata().unwrap().file_type().is_dir()),
            unix::AtEntry::Regular(_, _) => panic!("sub classified as a regular file"),
        }
        for name in ["lnk_file", "lnk_dir", "lnk_bad", "pipe.fifo", "s.sock"] {
            assert_error(
                unix::openat_entry_nofollow(&d, name, name),
                PackImportErrorKind::Special,
                name,
            );
        }
        assert_error(
            unix::openat_entry_nofollow(&d, "missing", "missing"),
            PackImportErrorKind::Io,
            "missing",
        );
        for name in [".", "..", "a/b", "a\0b"] {
            assert_error(
                unix::openat_entry_nofollow(&d, name, name),
                PackImportErrorKind::Traversal,
                name,
            );
        }

        unix::openat_nofollow(&d, "reg.bin", false, "reg.bin").unwrap();
        for name in ["sub", "pipe.fifo", "s.sock", "lnk_file", "lnk_bad"] {
            assert_error(
                unix::openat_nofollow(&d, name, false, name),
                PackImportErrorKind::Special,
                name,
            );
        }
        unix::openat_nofollow(&d, "sub", true, "sub").unwrap();
        for name in ["reg.bin", "pipe.fifo", "s.sock", "lnk_dir", "lnk_bad"] {
            assert_error(
                unix::openat_nofollow(&d, name, true, name),
                PackImportErrorKind::Special,
                name,
            );
        }

        unix::open_path_regular_nofollow(&dir.join("reg.bin"), "reg").unwrap();
        for name in ["lnk_file", "lnk_bad", "sub", "pipe.fifo", "s.sock"] {
            assert_error(
                unix::open_path_regular_nofollow(&dir.join(name), name),
                PackImportErrorKind::Special,
                name,
            );
        }
        assert_error(
            unix::open_path_regular_nofollow(&dir.join("missing"), "missing"),
            PackImportErrorKind::Io,
            "missing",
        );
        assert_error(
            unix::open_dir_path(&dir.join("pipe.fifo")),
            PackImportErrorKind::Io,
            "pipe.fifo",
        );

        unix::mkdirat_or_open_dir(&d, "created").unwrap();
        assert!(dir.join("created").is_dir());
        unix::mkdirat_or_open_dir(&d, "created").unwrap();
        for name in ["lnk_dir", "lnk_bad", "reg.bin", "pipe.fifo", "s.sock"] {
            assert_error(
                unix::mkdirat_or_open_dir(&d, name),
                PackImportErrorKind::Special,
                name,
            );
        }

        drop(socket);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn unix_domain_socket_in_pack_is_refused_as_special() {
        let pack = test_root("sock");
        let socket = std::os::unix::net::UnixListener::bind(pack.join("s.png")).unwrap();
        let out = test_bundle("sock_out");
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Special, "{err}");

        drop(socket);
        let _ = fs::remove_dir_all(&pack);
        let _ = fs::remove_dir_all(out.parent().unwrap());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn skipped_unreadable_entries_pass_and_special_dot_entries_refuse() {
        use std::os::unix::{
            fs::{symlink, PermissionsExt},
            net::UnixListener,
        };

        let pack = test_root("skip");
        fs::write(pack.join("ok.png"), valid_png(32, 32)).unwrap();

        let readme = pack.join("README.md");
        fs::write(&readme, b"docs").unwrap();
        fs::set_permissions(&readme, fs::Permissions::from_mode(0o000)).unwrap();

        let ds_store = pack.join(".DS_Store");
        fs::write(&ds_store, b"finder").unwrap();
        fs::set_permissions(&ds_store, fs::Permissions::from_mode(0o000)).unwrap();

        let dot_git = pack.join(".git");
        fs::create_dir(&dot_git).unwrap();
        fs::write(dot_git.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
        fs::set_permissions(&dot_git, fs::Permissions::from_mode(0o000)).unwrap();

        let out_ok = test_bundle("skip_unreadable_ok");
        let report = compile_pack(&pack, &out_ok, licensed_spec(), None, false)
            .expect("unreadable README.md, .DS_Store, and .git/ must be skipped before open");
        assert_eq!(report.assets, 1);

        fs::set_permissions(&ds_store, fs::Permissions::from_mode(0o600)).unwrap();
        fs::remove_file(&ds_store).unwrap();
        symlink("ok.png", &ds_store).unwrap();
        let out_lnk = test_bundle("skip_unreadable_lnk");
        let err = compile_pack(&pack, &out_lnk, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Special, "{err}");

        fs::remove_file(&ds_store).unwrap();
        let socket = UnixListener::bind(&ds_store).unwrap();
        let out_sock = test_bundle("skip_unreadable_sock");
        let err = compile_pack(&pack, &out_sock, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Special, "{err}");

        drop(socket);
        let _ = fs::set_permissions(&readme, fs::Permissions::from_mode(0o600));
        let _ = fs::set_permissions(&dot_git, fs::Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(&pack);
        for out in [&out_ok, &out_lnk, &out_sock] {
            let _ = fs::remove_dir_all(out.parent().unwrap());
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn walk_entry_dt_unknown_and_preopen_decisions() {
        fn must_not_open() -> Result<(), PackImportError> {
            panic!("open must not be called")
        }
        fn io_err() -> Result<(), PackImportError> {
            Err(PackImportError::new(PackImportErrorKind::Io, "eacces"))
        }
        fn special_err() -> Result<(), PackImportError> {
            Err(PackImportError::new(PackImportErrorKind::Special, "special"))
        }
        fn ok_open() -> Result<&'static str, PackImportError> {
            Ok("opened")
        }

        // DT_UNKNOWN + Io on dot or skip_entry names -> SkipUnreadable (logged & skipped).
        for (name, rel) in [
            ("README.md", "README.md"),
            (".DS_Store", ".DS_Store"),
            (".git", ".git"),
            (".env", "sub/.env"),
        ] {
            match open_walk_entry(name, rel, unix::DT_UNKNOWN, io_err).unwrap() {
                EntryOpen::SkipUnreadable(err) => assert_eq!(err.kind, PackImportErrorKind::Io, "{rel}: {err}"),
                other => panic!("{rel}: expected SkipUnreadable(Io), got {other:?}"),
            }
        }

        // DT_UNKNOWN + Io on a real asset -> Err(Io).
        let err = open_walk_entry("ok.png", "ok.png", unix::DT_UNKNOWN, io_err).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Io, "{err}");

        // DT_UNKNOWN + Special on ignored/dot names -> Err(Special), never skipped.
        for name in [".DS_Store", "README.md", ".git"] {
            let err = open_walk_entry(name, name, unix::DT_UNKNOWN, special_err).unwrap_err();
            assert_eq!(err.kind, PackImportErrorKind::Special, "{name}: {err}");
        }

        // Non-DT_UNKNOWN (DT_DIR) + Io on a non-dot skip_entry-named directory -> Err(Io).
        let err = open_walk_entry("license", "license", unix::DT_DIR, io_err).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Io, "{err}");

        // DT_UNKNOWN + Ok -> Opened (delegated to post-open classification).
        for name in ["ok.png", "README.md", ".git"] {
            match open_walk_entry(name, name, unix::DT_UNKNOWN, ok_open).unwrap() {
                EntryOpen::Opened(v) => assert_eq!(v, "opened"),
                other => panic!("{name}: expected Opened, got {other:?}"),
            }
        }

        // Pre-open skips never call open (even under root CAP_DAC_OVERRIDE).
        for (name, d_type) in [
            (".git", unix::DT_DIR),
            ("README.md", unix::DT_REG),
            (".DS_Store", unix::DT_REG),
        ] {
            match open_walk_entry(name, name, d_type, must_not_open).unwrap() {
                EntryOpen::Skip => {}
                other => panic!("{name}: expected Skip, got {other:?}"),
            }
        }

        // Pre-open refusals never call open.
        let err = open_walk_entry(".env", ".env", unix::DT_REG, must_not_open).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Unsupported, "{err}");
        for (name, d_type) in [
            (".DS_Store", unix::DT_LNK),
            ("README.md", unix::DT_LNK),
            (".DS_Store", 1u8),  // DT_FIFO
            (".DS_Store", 12u8), // DT_SOCK
        ] {
            let err = open_walk_entry(name, name, d_type, must_not_open).unwrap_err();
            assert_eq!(err.kind, PackImportErrorKind::Special, "{name}/{d_type}: {err}");
        }

        // Admitted d_type entries invoke open and propagate Ok / Err.
        for (name, d_type) in [("sub", unix::DT_DIR), ("ok.png", unix::DT_REG)] {
            match open_walk_entry(name, name, d_type, ok_open).unwrap() {
                EntryOpen::Opened(v) => assert_eq!(v, "opened"),
                other => panic!("{name}: expected Opened, got {other:?}"),
            }
        }
        assert_eq!(
            open_walk_entry("ok.png", "ok.png", unix::DT_REG, io_err).unwrap_err().kind,
            PackImportErrorKind::Io
        );
        assert_eq!(
            open_walk_entry("ok.png", "ok.png", unix::DT_REG, special_err).unwrap_err().kind,
            PackImportErrorKind::Special
        );
    }

    #[test]
    fn traversal_symlink_and_collisions_refuse() {
        assert_eq!(
            classify_rel("a/../../../etc/passwd.png")
                .unwrap_err()
                .kind,
            PackImportErrorKind::Traversal
        );

        let pack = test_root("coll");
        let out = test_bundle("coll_out");
        fs::write(pack.join("foo bar.png"), png_header(32, 32)).unwrap();
        fs::write(pack.join("foo-bar.png"), png_header(32, 32)).unwrap();
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Collision);

        let pack = test_root("dupkey");
        fs::create_dir_all(pack.join("a")).unwrap();
        fs::write(pack.join("a/x.png"), valid_png(32, 32)).unwrap();
        fs::write(pack.join("a/x.jpeg"), jpeg_solid(32, 32)).unwrap();
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Collision);

        #[cfg(unix)]
        {
            let pack = test_root("link");
            let outside = test_root("outside");
            fs::write(outside.join("secret.png"), png_header(32, 32)).unwrap();
            std::os::unix::fs::symlink(outside.join("secret.png"), pack.join("escape.png")).unwrap();
            let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
            assert_eq!(err.kind, PackImportErrorKind::Special);

            let pack = test_root("linkdir");
            std::os::unix::fs::symlink(&outside, pack.join("ext")).unwrap();
            let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
            assert_eq!(err.kind, PackImportErrorKind::Special);

            let pack = test_root("linkin");
            fs::write(pack.join("real.png"), png_header(32, 32)).unwrap();
            std::os::unix::fs::symlink(pack.join("real.png"), pack.join("alias.png")).unwrap();
            let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
            assert_eq!(err.kind, PackImportErrorKind::Special);
        }
    }

    #[test]
    fn case_fold_collision_is_detected() {
        let files = vec![
            discovered("Textures/Hull.PNG", "textures/hull.png", "textures/hull"),
            discovered("textures/hull.png", "textures/hull.png", "textures/hull"),
        ];
        let err = detect_collisions(&files).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Collision);
        assert!(err.to_string().contains("case-fold") || err.to_string().contains("collides"));

        let mixed_sep = vec![
            discovered("Models\\Box.PNG", "models/box.png", "models/box"),
            discovered("models/box.png", "models/box.png", "models/box"),
        ];
        assert_eq!(
            detect_collisions(&mixed_sep).unwrap_err().kind,
            PackImportErrorKind::Collision
        );

        let ok = vec![
            discovered("textures/hull.png", "textures/hull.png", "textures/hull"),
            discovered("textures/detail.png", "textures/detail.png", "textures/detail"),
        ];
        assert!(detect_collisions(&ok).is_ok());
    }

    #[test]
    fn changed_file_during_hash_refuses() {
        let pack = test_root("chg");
        let path = pack.join("x.png");
        let original = png_header(16, 16);
        fs::write(&path, &original).unwrap();
        let snap = FileSnapshot::from_meta(&fs::metadata(&path).unwrap());
        let (blob, len) = hash_regular_file(&path, &snap, "x.png").unwrap();
        assert_eq!(len, original.len() as u64);
        assert_eq!(blob, BlobId::hash_of(&original));

        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[0u8; 32]).unwrap();
        drop(f);
        let err = hash_regular_file(&path, &snap, "x.png").unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Changed);

        // Same-length overwrite: metadata or byte count must still fail closed.
        fs::write(&path, &original).unwrap();
        let snap = FileSnapshot::from_meta(&fs::metadata(&path).unwrap());
        let other = png_header(32, 32);
        assert_eq!(other.len(), original.len());
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&path, &other).unwrap();
        let err = hash_regular_file(&path, &snap, "x.png").unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Changed);

        let err = recheck_unchanged(&path, &snap, "x.png").unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Changed);
    }

    #[test]
    fn supported_mixed_pack_round_trips_canonical_documents() {
        let pack = test_root("mix");
        let out = test_bundle("mix_out");
        write_mixed_pack(&pack, shared_mp4());
        let report = compile(&pack, &out, licensed_spec());
        assert_eq!(
            report.assets, 3,
            "wav, mp4, mesh — a mesh pack's textures/ atlases ride with the meshes"
        );
        assert!(report.blobs >= 4, "three assets + mesh thumbnail");

        let source_bytes = fs::read(&report.source_path).unwrap();
        let manifest_bytes = fs::read(&report.manifest_path).unwrap();
        let plan = fs::read(&report.plan_path).unwrap();
        let collection = SourceCollection::from_canonical_bytes(&source_bytes).unwrap();
        let manifest = ImportManifest::from_canonical_bytes(&manifest_bytes).unwrap();
        assert_eq!(collection.id, "kenney");
        assert_eq!(collection.terms.license, "CC-BY-4.0");
        assert_eq!(
            collection.terms.terms_digest,
            Some(sha256(b"CC-BY-4.0 legal text for pack_import tests"))
        );
        assert_eq!(manifest.source_id, "kenney");
        assert_eq!(manifest.pack_name, "space-kit");
        assert_eq!(manifest.pack_version, "1.0");
        assert_eq!(manifest.policy_version, IMPORT_ASSET_ID_POLICY_V1);
        assert_eq!(manifest.rights, collection.terms);
        assert_eq!(manifest.source_collection, collection.digest().unwrap());

        let has = |k| manifest.assets.iter().any(|a| a.kind == k);
        assert!(
            !has(AssetKind::Texture),
            "textures/ images of a mesh pack are never standalone catalog rows"
        );
        assert!(has(AssetKind::Audio));
        assert!(has(AssetKind::Video));
        assert!(has(AssetKind::Mesh));

        let mesh = manifest
            .assets
            .iter()
            .find(|a| a.kind == AssetKind::Mesh)
            .unwrap();
        assert_eq!(mesh.key.as_str(), "models/box");
        assert_eq!(mesh.metrics.triangles, 1);
        assert!(mesh.metrics.vertices >= 3);
        let thumb = mesh.thumbnail.as_ref().unwrap();
        assert_eq!(thumb.path, "models/box.png");
        assert_eq!(thumb.meta.width, 512);

        let audio = manifest
            .assets
            .iter()
            .find(|a| a.kind == AssetKind::Audio)
            .unwrap();
        assert_eq!(audio.metrics.media_millis, 100);

        let video = manifest
            .assets
            .iter()
            .find(|a| a.kind == AssetKind::Video)
            .unwrap();
        assert!(video.metrics.media_millis > 200, "probed video duration");

        let plan_text = String::from_utf8(plan).unwrap();
        assert!(plan_text.starts_with(&format!("{{\"schema\":\"{PLAN_SCHEMA}\"")));
        assert!(plan_text.contains("kenney/space-kit/models/box"));
        assert!(plan_text.contains("sha256:"));

        // Same-stem preview is a thumbnail, not a sixth texture named models/box.
        assert_eq!(
            manifest
                .assets
                .iter()
                .filter(|a| a.key.as_str() == "models/box")
                .count(),
            1
        );
    }

    /// The progress callback: fires once before any file (files_done=0,
    /// totals already known), once per file after (monotonic, ending at the
    /// discovered total), and reports a plausible byte count along the way
    /// — what a caller like the classic-pack publish path drives its status
    /// line from.
    #[test]
    fn compile_with_progress_reports_a_monotonic_file_and_byte_count() {
        let pack = test_root("progress");
        let out = test_bundle("progress_out");
        write_mixed_pack(&pack, shared_mp4());

        let mut calls: Vec<CompileProgress> = Vec::new();
        let mut cb = |p: CompileProgress| calls.push(p);
        compile_pack_with_progress(&pack, &out, licensed_spec(), None, false, Some(&mut cb))
            .expect("compile with progress");

        assert!(calls.len() >= 2, "at least an initial call and one per file");
        let first = &calls[0];
        assert_eq!(first.files_done, 0, "no file has landed on the very first call");
        assert!(first.files_total > 0, "the total is known from the first call");
        assert_eq!(first.bytes_done, 0);
        assert!(first.bytes_total > 0);

        let last = calls.last().unwrap();
        assert_eq!(last.files_done, last.files_total, "the run ends fully counted");
        assert_eq!(last.files_total, first.files_total, "the total never changes mid-run");
        assert_eq!(last.bytes_total, first.bytes_total, "the byte total never changes mid-run");
        assert!(last.bytes_done > 0);
        assert!(last.bytes_done <= last.bytes_total);

        // Every field only ever moves forward.
        for pair in calls.windows(2) {
            assert!(pair[1].files_done >= pair[0].files_done, "files_done went backwards");
            assert!(pair[1].bytes_done >= pair[0].bytes_done, "bytes_done went backwards");
        }
    }

    #[test]
    fn reruns_are_byte_identical() {
        let pack = test_root("det");
        let out_a = test_bundle("det_a");
        let out_b = test_bundle("det_b");
        write_mixed_pack(&pack, shared_mp4());
        let a = compile(&pack, &out_a, licensed_spec());
        let b = compile(&pack, &out_b, licensed_spec());
        assert_eq!(a.source_digest, b.source_digest);
        assert_eq!(a.import_revision, b.import_revision);
        assert_eq!(
            fs::read(&a.source_path).unwrap(),
            fs::read(&b.source_path).unwrap()
        );
        assert_eq!(
            fs::read(&a.manifest_path).unwrap(),
            fs::read(&b.manifest_path).unwrap()
        );
        assert_eq!(
            fs::read(&a.plan_path).unwrap(),
            fs::read(&b.plan_path).unwrap()
        );
        // Rewrite the same bytes and compile again — still identical.
        write_mixed_pack(&pack, shared_mp4());
        let out_c = test_bundle("det_c");
        let c = compile(&pack, &out_c, licensed_spec());
        assert_eq!(
            fs::read(&a.manifest_path).unwrap(),
            fs::read(&c.manifest_path).unwrap()
        );

        // Different creation order, same payload bytes → same documents.
        let pack_rev = test_root("det_rev");
        write_mixed_pack_reversed(&pack_rev, shared_mp4());
        let out_d = test_bundle("det_d");
        let d = compile(&pack_rev, &out_d, licensed_spec());
        assert_eq!(
            fs::read(&a.source_path).unwrap(),
            fs::read(&d.source_path).unwrap()
        );
        assert_eq!(
            fs::read(&a.manifest_path).unwrap(),
            fs::read(&d.manifest_path).unwrap()
        );
        assert_eq!(fs::read(&a.plan_path).unwrap(), fs::read(&d.plan_path).unwrap());

        let manifest_bytes = fs::read(&a.manifest_path).unwrap();
        let back = ImportManifest::from_canonical_bytes(&manifest_bytes).unwrap();
        assert_eq!(back.to_canonical_bytes().unwrap(), manifest_bytes);
        let source_bytes = fs::read(&a.source_path).unwrap();
        let back = SourceCollection::from_canonical_bytes(&source_bytes).unwrap();
        assert_eq!(back.to_canonical_bytes().unwrap(), source_bytes);
    }

    #[test]
    fn missing_mesh_thumbnail_refuses_instead_of_inventing() {
        let pack = test_root("nothumb");
        let out = test_bundle("nothumb_out");
        fs::write(pack.join("solo.glb"), tiny_glb()).unwrap();
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed);
        assert!(err.to_string().contains("thumbnail"));
    }

    #[test]
    fn config_file_drives_a_compile() {
        let pack = test_root("cfgpack");
        let out = test_bundle("cfgout");
        fs::write(pack.join("panel.png"), valid_png(64, 64)).unwrap();
        let cfg_dir = test_root("cfgdir");
        let cfg = cfg_dir.join("source.json");
        fs::write(
            &cfg,
            format!(
                r#"{{"source_id":"kenney","source_title":"Kenney game assets","pack_name":"ui-pack","pack_version":"2.0","license":"CC-BY-4.0","terms_digest":"{}","terms_url":"https://creativecommons.org/licenses/by/4.0/","credits":"Kenney (kenney.nl)","source":"https://kenney.nl/assets/ui-pack","redistribution":"allowed","derivatives":"allowed"}}"#,
                terms_hex()
            ),
        )
        .unwrap();
        let report = compile_pack(&pack, &out, PackSourceSpec::default(), Some(&cfg), false).unwrap();
        let bytes = fs::read(report.manifest_path).unwrap();
        let manifest = ImportManifest::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(manifest.pack_name, "ui-pack");
        assert_eq!(manifest.pack_version, "2.0");
        assert_eq!(manifest.rights.redistribution, Redistribution::Allowed);
    }

    #[test]
    fn hostile_png_header_only_and_truncated_wav_and_trackless_mp4_refuse() {
        let out = test_bundle("hostile_media_out");
        let pack = test_root("hostile_png");
        fs::write(pack.join("x.png"), png_header(512, 512)).unwrap();
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed);
        assert!(err.to_string().contains("png") || err.to_string().contains("IDAT"));

        let pack = test_root("hostile_wav");
        let mut wav = wav_pcm16(&[(1, 1); 100], 8_000);
        wav.truncate(20);
        fs::write(pack.join("x.wav"), wav).unwrap();
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed);

        let pack = test_root("hostile_mp4");
        fs::write(pack.join("x.mp4"), tiny_mp4(1_500)).unwrap();
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed);
    }

    #[test]
    fn pack_wide_preview_is_not_a_mesh_thumbnail() {
        let pack = test_root("prevonly");
        let out = test_bundle("prevonly_out");
        fs::write(pack.join("solo.glb"), tiny_glb()).unwrap();
        fs::write(pack.join("preview.png"), valid_png(512, 512)).unwrap();
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed);
        assert!(err.to_string().contains("thumbnail"));
    }

    #[test]
    fn refuse_symlink_output_leaf_and_oversized_config_and_deep_walk() {
        let pack = test_root("leafpack");
        let out = test_bundle("leafout");
        fs::write(pack.join("ok.png"), valid_png(32, 32)).unwrap();
        compile(&pack, &out, licensed_spec());
        #[cfg(unix)]
        {
            let dest = out.join(SOURCE_COLLECTION_FILE);
            fs::remove_file(&dest).unwrap();
            std::os::unix::fs::symlink("/tmp/nowhere", &dest).unwrap();
            let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
            assert_eq!(err.kind, PackImportErrorKind::Special);
        }

        let cfg_dir = test_root("bigcfg");
        let cfg = cfg_dir.join("source.json");
        fs::write(&cfg, vec![b'x'; MAX_SOURCE_CONFIG_BYTES as usize + 8]).unwrap();
        let err = compile_pack(&pack, &out, PackSourceSpec::default(), Some(&cfg), false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Config);

        let deep = test_root("deep");
        let mut cur = deep.clone();
        for i in 0..=MAX_WALK_DEPTH {
            cur = cur.join(format!("d{i}"));
        }
        fs::create_dir_all(&cur).unwrap();
        fs::write(cur.join("x.png"), valid_png(16, 16)).unwrap();
        let err = compile_pack(&deep, &test_bundle("deep_out"), licensed_spec(), None, false)
            .unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Content);
        assert!(err.to_string().contains("depth"));
    }

    #[test]
    fn plan_projects_full_rights_and_reverify_and_trims_spec() {
        let pack = test_root("planpack");
        let out = test_bundle("planout");
        fs::write(pack.join("ok.png"), valid_png(16, 16)).unwrap();
        let mut spec = licensed_spec();
        spec.license = Some("  CC-BY-4.0  ".into());
        spec.license_revision = Some("  2024-01  ".into());
        spec.source_archive = Some(terms_hex());
        let report = compile(&pack, &out, spec);
        let plan = String::from_utf8(fs::read(report.plan_path).unwrap()).unwrap();
        assert!(plan.contains("terms_digest"));
        assert!(plan.contains("license_revision"));
        assert!(plan.contains("source_archive"));
        assert!(plan.contains("reverify_digest"));
        assert!(plan.contains(UPLOADER_REVERIFY));
        assert!(plan.contains("AssetClient::upload_blob"));
        assert!(plan.contains("AssetClient::register_source_collection"));
        assert!(plan.contains("AssetClient::run_import"));
        assert!(plan.contains("\"namespace\":\"kenney\""));
        assert!(plan.contains("\"op\":\"upload_blob\""));
        assert!(plan.contains("\"op\":\"register_source_collection\""));
        assert!(plan.contains("\"op\":\"run_import\""));
        assert!(plan.contains("pack-root-relative"));
        let register = plan.find("\"op\":\"register_source_collection\"").unwrap();
        let upload = plan.find("\"op\":\"upload_blob\"").unwrap();
        let import = plan.find("\"op\":\"run_import\"").unwrap();
        assert!(
            register < upload && upload < import,
            "upload plan must be register_source -> upload_blob -> run_import"
        );
        assert!(plan.contains("expect_digest"));
        assert!(plan.contains("expect_revision"));
        let manifest = ImportManifest::from_canonical_bytes(&fs::read(report.manifest_path).unwrap())
            .unwrap();
        assert_eq!(manifest.rights.license, "CC-BY-4.0");
        assert_eq!(manifest.rights.license_revision, "2024-01");
        assert!(manifest.rights.source_archive.is_some());
    }

    #[test]
    fn pack_identity_flags_are_detected() {
        assert!(!PackSourceSpec::default().has_pack_identity());
        let mut spec = PackSourceSpec::default();
        spec.source_id = Some("kenney".into());
        assert!(spec.has_pack_identity());
    }

    #[test]
    fn exact_out_bundle_is_idempotent_and_divergent_or_extra_refuses() {
        let pack = test_root("idemp_pack");
        fs::write(pack.join("ok.png"), valid_png(16, 16)).unwrap();
        let out = test_bundle("idemp_out");
        let first = compile(&pack, &out, licensed_spec());
        let source_a = fs::read(&first.source_path).unwrap();
        let manifest_a = fs::read(&first.manifest_path).unwrap();
        let plan_a = fs::read(&first.plan_path).unwrap();
        let second = compile(&pack, &out, licensed_spec());
        assert_eq!(fs::read(&second.source_path).unwrap(), source_a);
        assert_eq!(fs::read(&second.manifest_path).unwrap(), manifest_a);
        assert_eq!(fs::read(&second.plan_path).unwrap(), plan_a);

        fs::write(&first.source_path, b"divergent-bytes").unwrap();
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert!(
            err.to_string().contains("diverge") || err.kind == PackImportErrorKind::Io,
            "{err}"
        );

        let out2 = test_bundle("idemp_extra");
        compile(&pack, &out2, licensed_spec());
        fs::write(out2.join("extra.txt"), b"nope").unwrap();
        let err = compile_pack(&pack, &out2, licensed_spec(), None, false).unwrap_err();
        assert!(err.to_string().contains("extra") || err.kind == PackImportErrorKind::Io);

        let empty_existing = test_root("empty_existing_out");
        let err = compile_pack(&pack, &empty_existing, licensed_spec(), None, false).unwrap_err();
        assert!(
            err.to_string().contains("choose another path")
                || err.to_string().contains("bundle")
                || err.kind == PackImportErrorKind::Io
        );

        let file_out = test_root("file_out_parent").join("not-a-dir");
        fs::write(&file_out, b"nope").unwrap();
        let err = compile_pack(&pack, &file_out, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Special);

        #[cfg(unix)]
        {
            let target = test_root("symlink_out_target");
            let link = test_root("symlink_out_parent").join("link");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            let err = compile_pack(&pack, &link, licensed_spec(), None, false).unwrap_err();
            assert_eq!(err.kind, PackImportErrorKind::Special);
        }
    }

    #[test]
    fn per_dir_enumeration_stops_before_unbounded_collect() {
        let pack = test_root("manyents");
        for i in 0..=MAX_DIR_ENTRIES {
            fs::write(pack.join(format!("n{i}.txt")), b"skip").unwrap();
        }
        let err = compile_pack(&pack, &test_bundle("manyents_out"), licensed_spec(), None, false)
            .unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Content);
        assert!(err.to_string().contains("entries"), "{err}");
    }

    /// The per-directory bound still exists and still refuses readably —
    /// expressed against the constant, so it holds at whatever the bound is.
    #[test]
    fn a_flat_kit_of_multi_file_models_is_refused_by_the_per_dir_cap() {
        let pack = test_root("flatkit");
        // Comfortably over the cap at five entries per model, and well
        // under MAX_WALK_ENTRIES so the per-directory bound is what bites.
        let models = MAX_DIR_ENTRIES / 4;
        for i in 0..models {
            let stem = format!("brick-{i:04}");
            fs::write(pack.join(format!("{stem}.png")), valid_png(4, 4)).unwrap();
            fs::write(pack.join(format!("{stem}.ao.png")), valid_png(4, 4)).unwrap();
            fs::write(pack.join(format!("{stem}.aomesh")), b"aomesh").unwrap();
            fs::write(pack.join(format!("{stem}.shadowsdf")), b"sdf").unwrap();
            fs::write(pack.join(format!("{stem}.jpg")), b"jpeg").unwrap();
        }
        let err = compile_pack(&pack, &test_bundle("flatkit_out"), licensed_spec(), None, false)
            .unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Content);
        assert!(
            err.to_string().contains(&format!("exceeds {MAX_DIR_ENTRIES} entries")),
            "{err}"
        );
        // The pack root is the one directory every pack has; it must not
        // print as a blank ("directory  exceeds 1024 entries").
        assert!(err.to_string().contains("<pack root>"), "{err}");
        assert!(
            !err.to_string().contains("directory  "),
            "the pack root printed as an empty name: {err}"
        );
    }

    /// The bound must admit a real flat vendor kit. Kenney ships every
    /// model of a kit in one folder, and a STAGED model is five directory
    /// entries sharing one entry key — `.glb`, `.png`, and the `.aomesh` /
    /// `.ao.png` / `.shadowsdf` the AO bake writes beside it (pinned by
    /// `baked_sidecars_attach_to_their_glb_as_derived_roles`).
    ///
    /// At `MAX_DIR_ENTRIES = 1024` that meant ~204 models, and brick-kit
    /// (296) and nature-kit (329) were refused outright: "content:
    /// directory <pack root> exceeds 1024 entries". This is the arithmetic
    /// that refusal was, so the bound cannot quietly drift back under it.
    #[test]
    fn the_per_dir_cap_admits_a_real_flat_vendor_kit() {
        /// Kenney's nature-kit, the largest kit on the LOAD surface.
        const LARGEST_KIT_MODELS: usize = 329;
        /// glb + thumbnail + aomesh + ao.png + shadowsdf.
        const STAGED_ENTRIES_PER_MODEL: usize = 5;
        let needed = LARGEST_KIT_MODELS * STAGED_ENTRIES_PER_MODEL;
        assert!(
            MAX_DIR_ENTRIES >= needed,
            "a flat {LARGEST_KIT_MODELS}-model kit stages {needed} entries in the pack root, \
             but one directory is capped at {MAX_DIR_ENTRIES}"
        );
        // Strictly under the whole-tree bound, so a pack that is flat AND
        // huge still meets the per-directory refusal rather than the total.
        assert!(MAX_DIR_ENTRIES < MAX_WALK_ENTRIES);
        // And the bound stays a bound: it is not quietly wider than the
        // shape the content contract itself permits.
        assert!(
            MAX_DIR_ENTRIES
                <= makepad_asset_data::limits::MAX_IMPORT_ASSETS
                    * makepad_asset_data::limits::MAX_IMPORT_FILES_PER_ASSET
        );
    }

    /// A directory segment IS part of the entry key, and the key IS the
    /// catalog alias (`{source_id}/{pack_name}/{key}` — `alias_for`).
    ///
    /// So "shard a big flat pack into subdirectories to fit the per-dir
    /// cap" is not a layout detail: it renames every asset in the pack and
    /// breaks re-import-as-a-new-revision for all of them. Pinned so the
    /// cost of that idea shows up as a failing test, not as a silently
    /// re-identified catalog.
    #[test]
    fn a_directory_segment_becomes_part_of_the_entry_key_and_the_alias() {
        let (flat_path, flat_key, _) = classify_rel("brick-a.png").unwrap();
        assert_eq!(flat_path, "brick-a.png");
        assert_eq!(flat_key, "brick-a");

        let (sharded_path, sharded_key, _) = classify_rel("shard-00/brick-a.png").unwrap();
        assert_eq!(sharded_path, "shard-00/brick-a.png");
        assert_eq!(
            sharded_key, "shard-00/brick-a",
            "sharding changes the entry key, and the key is the asset's identity"
        );
        assert_ne!(flat_key, sharded_key);

        // …and that difference reaches the PUBLISHED alias verbatim, through
        // the real compile — `alias_for` is `{source_id}/{pack_name}/{key}`.
        let alias_of = |rel: &str, name: &str| -> String {
            let pack = test_root(name);
            let path = pack.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, tiny_glb()).unwrap();
            fs::write(path.with_extension("png"), valid_png(512, 512)).unwrap();
            let report =
                compile_pack(&pack, &test_bundle(name), licensed_spec(), None, false).unwrap();
            let manifest =
                ImportManifest::from_canonical_bytes(&fs::read(&report.manifest_path).unwrap())
                    .unwrap();
            manifest
                .alias_for(&manifest.assets[0].key)
                .unwrap()
                .as_str()
                .to_string()
        };
        let flat_alias = alias_of("brick-a.glb", "alias_flat");
        let sharded_alias = alias_of("shard-00/brick-a.glb", "alias_sharded");
        assert!(flat_alias.ends_with("/brick-a"), "{flat_alias}");
        assert!(
            sharded_alias.ends_with("/shard-00/brick-a"),
            "a shard directory lands in the catalog alias: {sharded_alias}"
        );
        assert_ne!(flat_alias, sharded_alias);
    }

    #[test]
    fn mp4_probe_refuses_digest_mismatch_without_using_pack_path() {
        let bytes = b"not-a-trusted-mp4";
        let wrong = BlobId::hash_of(b"other");
        let err = probe_mp4_trusted(bytes, wrong, "video/clip.mp4").unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Changed);
        assert!(err.to_string().contains("digest"));
    }

    #[cfg(unix)]
    #[test]
    fn parent_symlink_cannot_escape_pack_root() {
        let pack = test_root("escape_pack");
        let outside = test_root("escape_out");
        fs::create_dir_all(outside.join("nested")).unwrap();
        fs::write(outside.join("nested/x.png"), valid_png(16, 16)).unwrap();
        fs::create_dir_all(pack.join("keep")).unwrap();
        fs::write(pack.join("keep/ok.png"), valid_png(16, 16)).unwrap();
        std::os::unix::fs::symlink(&outside, pack.join("nested")).unwrap();
        let err = compile_pack(
            &pack,
            &test_bundle("escape_bundle"),
            licensed_spec(),
            None,
            false,
        )
        .unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Special);

        // Intermediate component replaced by a symlink after a real tree existed.
        let pack = test_root("swap_pack");
        fs::create_dir_all(pack.join("a/b")).unwrap();
        fs::write(pack.join("a/b/x.png"), valid_png(16, 16)).unwrap();
        fs::remove_dir_all(pack.join("a")).unwrap();
        std::os::unix::fs::symlink(&outside, pack.join("a")).unwrap();
        let err = compile_pack(
            &pack,
            &test_bundle("swap_bundle"),
            licensed_spec(),
            None,
            false,
        )
        .unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Special);
    }

    fn tiny_glb_with_uri(uri: &str) -> Vec<u8> {
        let positions: [f32; 9] = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let mut bin: Vec<u8> = Vec::new();
        for f in positions {
            bin.extend_from_slice(&f.to_le_bytes());
        }
        let escaped = uri.replace('\\', "\\\\").replace('"', "\\\"");
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},
            "nodes":[{{"mesh":0}}],
            "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}}}}]}}],
            "accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}}],
            "bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":{}}}],
            "buffers":[{{"byteLength":{}}}],
            "images":[{{"uri":"{escaped}"}}]}}"#,
            bin.len(),
            bin.len()
        );
        let mut json_bytes = json.into_bytes();
        while json_bytes.len() % 4 != 0 {
            json_bytes.push(b' ');
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(b"JSON");
        out.extend_from_slice(&json_bytes);
        out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(&bin);
        out
    }

    /// The shape Kenney's retro kits ship: ONE mesh whose primitives each
    /// carry their own material, each material its own EMBEDDED picture.
    /// `barrels.glb` is barrel+planks, `cliff-corner.glb` is rock+grass;
    /// across the two kits there are 157 of these, up to four ways.
    ///
    /// Nothing here points outside the file — there is no external texture
    /// to publish as a separate blob, which is what the one-atlas rule is
    /// actually about.
    fn multi_texture_glb(images: usize) -> Vec<u8> {
        assert!((1..=4).contains(&images));
        let positions: [f32; 9] = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let uvs: [f32; 6] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0];
        let mut bin: Vec<u8> = Vec::new();
        for f in positions {
            bin.extend_from_slice(&f.to_le_bytes());
        }
        let uv_off = bin.len();
        for f in uvs {
            bin.extend_from_slice(&f.to_le_bytes());
        }
        // Each material's own picture, embedded in the BIN chunk.
        let mut views = vec![
            format!(
                r#"{{"buffer":0,"byteOffset":0,"byteLength":{}}}"#,
                uv_off
            ),
            format!(
                r#"{{"buffer":0,"byteOffset":{uv_off},"byteLength":{}}}"#,
                bin.len() - uv_off
            ),
        ];
        let mut image_defs = Vec::new();
        for i in 0..images {
            while bin.len() % 4 != 0 {
                bin.push(0);
            }
            let off = bin.len();
            let png = valid_png(8 + i as u32, 8);
            bin.extend_from_slice(&png);
            views.push(format!(
                r#"{{"buffer":0,"byteOffset":{off},"byteLength":{}}}"#,
                png.len()
            ));
            image_defs.push(format!(
                r#"{{"bufferView":{},"mimeType":"image/png","name":"mat{i}"}}"#,
                views.len() - 1
            ));
        }
        let prims: Vec<String> = (0..images)
            .map(|i| {
                format!(
                    r#"{{"attributes":{{"POSITION":0,"TEXCOORD_0":1}},"material":{i}}}"#
                )
            })
            .collect();
        let materials: Vec<String> = (0..images)
            .map(|i| {
                format!(
                    r#"{{"name":"mat{i}","pbrMetallicRoughness":{{"baseColorTexture":{{"index":{i}}}}}}}"#
                )
            })
            .collect();
        let textures: Vec<String> = (0..images)
            .map(|i| format!(r#"{{"source":{i}}}"#))
            .collect();
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},
            "nodes":[{{"mesh":0}}],
            "meshes":[{{"primitives":[{}]}}],
            "materials":[{}],
            "textures":[{}],
            "images":[{}],
            "accessors":[
                {{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}},
                {{"bufferView":1,"componentType":5126,"count":3,"type":"VEC2"}}],
            "bufferViews":[{}],
            "buffers":[{{"byteLength":{}}}]}}"#,
            prims.join(","),
            materials.join(","),
            textures.join(","),
            image_defs.join(","),
            views.join(","),
            bin.len()
        );
        let mut json_bytes = json.into_bytes();
        while json_bytes.len() % 4 != 0 {
            json_bytes.push(b' ');
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(b"JSON");
        out.extend_from_slice(&json_bytes);
        out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(&bin);
        out
    }

    /// A car's lamps come with the import: the compile bakes
    /// `car.lamps.glb` beside `car.glb` and publishes it as the render GLB;
    /// a second compile reuses the fresh bake and plans the same bytes.
    #[test]
    fn a_kit_car_imports_with_its_lamp_bake_as_the_render_glb() {
        let pack = test_root("lampcar");
        fs::write(pack.join("car.glb"), crate::vehicle_lamps::tests::kit_car()).unwrap();
        fs::write(pack.join("car.png"), valid_png(512, 512)).unwrap();
        let first = compile_pack(&pack, &test_bundle("lampcar_out_a"), licensed_spec(), None, false).expect("compiles");
        assert_eq!(first.assets, 1, "the bake is a sidecar, not an asset");
        assert!(pack.join("car.lamps.glb").is_file());
        let manifest = ImportManifest::from_canonical_bytes(&fs::read(&first.manifest_path).unwrap()).unwrap();
        let render = manifest.assets[0].files.iter().find(|f| f.file.role == FileRole::RenderGlb).unwrap();
        assert_eq!(render.path, "car.lamps.glb");
        let baked = fs::read(pack.join("car.lamps.glb")).unwrap();
        assert_eq!(render.file.blob, BlobId::hash_of(&baked));
        assert!(baked.windows(14).any(|w| w == b"headlight_left"));
        let second = compile_pack(&pack, &test_bundle("lampcar_out_b"), licensed_spec(), None, false).expect("recompiles");
        assert_eq!(fs::read(&first.plan_path).unwrap(), fs::read(&second.plan_path).unwrap());
    }

    /// The refusal that killed retro-fantasy-kit and retro-urban-kit whole:
    /// 157 of their 229 models bind more than one texture. Every one is
    /// SELF-CONTAINED — the pictures are embedded in the BIN chunk, there
    /// is no second pack file to publish — so nothing about the manifest
    /// was ever ambiguous, and both render lanes draw one call per image.
    #[test]
    fn a_multi_material_embedded_glb_imports_as_one_asset() {
        for images in [2usize, 4] {
            let pack = test_root(&format!("multitex{images}"));
            let out = test_bundle(&format!("multitex{images}_out"));
            fs::write(pack.join("barrels.glb"), multi_texture_glb(images)).unwrap();
            fs::write(pack.join("barrels.png"), valid_png(512, 512)).unwrap();
            let report = compile_pack(&pack, &out, licensed_spec(), None, false)
                .unwrap_or_else(|e| panic!("{images}-texture glb must import: {e}"));
            assert_eq!(report.assets, 1, "{images} textures is still one asset");
            assert!(report.skipped_models.is_empty(), "{:?}", report.skipped_models);
            let manifest =
                ImportManifest::from_canonical_bytes(&fs::read(&report.manifest_path).unwrap())
                    .unwrap();
            let roles: Vec<FileRole> =
                manifest.assets[0].files.iter().map(|f| f.file.role).collect();
            // The embedded pictures ride inside the mesh blob: no extra
            // Texture blob is published, whatever the material count.
            assert_eq!(roles, [FileRole::RenderGlb], "{roles:?}");
        }
    }

    /// The same rule refused every level this repo's OWN world writer
    /// produces — `write_glb_mesh_textured_parts` is one image per surface
    /// by construction, and it is what the Duke / Quake 2 / Quake 3 / Doom
    /// level importers call. A pack importer that cannot read our own
    /// output is not enforcing a contract, it is a bug.
    #[test]
    fn a_level_from_our_own_world_writer_passes_preflight() {
        let png_a = valid_png(8, 8);
        let png_b = valid_png(9, 8);
        let png_c = valid_png(10, 8);
        let pos = [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let idx = [0u32, 1, 2];
        let uvs = [[0.0f32, 0.0], [1.0, 0.0], [0.0, 1.0]];
        fn part<'a>(
            png: &'a [u8],
            pos: &'a [[f32; 3]],
            idx: &'a [u32],
            uvs: &'a [[f32; 2]],
        ) -> makepad_gltf::GlbTexturedPart<'a> {
            makepad_gltf::GlbTexturedPart {
                positions: pos,
                indices: idx,
                uvs,
                normals: None,
                colors: None,
                base_color_png: png,
                base_color_factor: None,
                lightmap_png: None,
                lightmap_uvs: None,
                detail_png: None,
                detail_scale: [1.0, 1.0],
                material_bytes: None,
            }
        }
        let glb = makepad_gltf::write_glb_mesh_textured_parts(
            &[
                part(&png_a, &pos, &idx, &uvs),
                part(&png_b, &pos, &idx, &uvs),
                part(&png_c, &pos, &idx, &uvs),
            ],
            true,
        );
        let uris = super::preflight_glb(&glb, "worlds/e1l1.glb")
            .expect("our own multi-surface level must preflight");
        assert!(uris.is_empty(), "a level embeds its surfaces: {uris:?}");
    }

    /// One texture FILE is still the limit, because a mesh asset carries
    /// exactly one `FileRole::Texture` blob and a second has nowhere to go.
    #[test]
    fn two_external_texture_files_are_still_refused() {
        let two = tiny_glb_with_two_uris("a.png", "b.png");
        let err = super::preflight_glb(&two, "twin.glb").unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Unsupported);
        assert!(err.to_string().contains("multi-texture"), "{err}");
    }

    /// A model the importer cannot represent costs that model, not the kit.
    #[test]
    fn one_unreadable_model_is_named_and_the_rest_of_the_pack_imports() {
        let pack = test_root("skipmodel");
        let out = test_bundle("skipmodel_out");
        for stem in ["good-a", "good-b"] {
            fs::write(pack.join(format!("{stem}.glb")), tiny_glb()).unwrap();
            fs::write(pack.join(format!("{stem}.png")), valid_png(512, 512)).unwrap();
        }
        // Two pack images for one mesh: representable in the pack, not in a
        // mesh asset's single Texture blob — a content refusal, per model.
        fs::write(pack.join("twin.glb"), tiny_glb_with_two_uris("a.png", "b.png")).unwrap();
        fs::write(pack.join("twin.png"), valid_png(512, 512)).unwrap();

        let report = compile_pack(&pack, &out, licensed_spec(), None, false)
            .expect("one bad model must not refuse the pack");
        assert_eq!(report.assets, 2, "both good models imported");
        assert_eq!(report.skipped_models.len(), 1);
        let (path, why) = &report.skipped_models[0];
        assert_eq!(path, "twin.glb");
        assert!(why.contains("multi-texture"), "{why}");

        let manifest =
            ImportManifest::from_canonical_bytes(&fs::read(&report.manifest_path).unwrap())
                .unwrap();
        let keys: Vec<&str> = manifest.assets.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(keys, ["good-a", "good-b"], "the skipped model is not published");
    }

    /// Quake 3's `sound/world/drone6.wav` is a complete, ordinary WAV whose
    /// odd-sized `data` chunk ends exactly at EOF with no trailing pad
    /// byte — the shape every practical reader accepts and real encoders
    /// routinely write. Refusing it cost the ENTIRE Quake 3 import
    /// ("wav pad extends past EOF"), one sound against a whole game.
    ///
    /// A pad that is genuinely missing BETWEEN chunks still refuses: there
    /// the absent byte misaligns everything after it.
    #[test]
    fn a_final_odd_chunk_may_omit_its_riff_pad_byte() {
        // 8-bit mono, an ODD number of frames so `data` is odd and last.
        let mut wav = Vec::new();
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&1u16.to_le_bytes()); // pcm
        fmt.extend_from_slice(&1u16.to_le_bytes()); // mono
        fmt.extend_from_slice(&8000u32.to_le_bytes());
        fmt.extend_from_slice(&8000u32.to_le_bytes());
        fmt.extend_from_slice(&1u16.to_le_bytes());
        fmt.extend_from_slice(&8u16.to_le_bytes()); // 8-bit
        let data: Vec<u8> = vec![0x80; 801];
        let body = 4 + (8 + fmt.len()) + (8 + data.len());
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(body as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
        wav.extend_from_slice(&fmt);
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        // No pad byte: the file ends exactly where the data ends.
        assert_eq!(wav.len() % 2, 1, "odd total, as the real file is");

        validate_wav(&wav, "sound/world/drone6.wav")
            .expect("a final odd chunk without its pad byte is a valid wav");

        // But an odd chunk followed by another chunk still needs its pad.
        let mut bad = wav.clone();
        bad.extend_from_slice(b"LIST");
        bad.extend_from_slice(&0u32.to_le_bytes());
        let err = validate_wav(&bad, "bad.wav").unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed);
    }

    /// A world's `.place` placements must REACH the catalog. Every other
    /// part of that lane was wired — the extension, `is_sidecar`, the media
    /// type, the skip-as-its-own-asset arm, the Source role, the parse on
    /// measure — but nothing attached it to the world GLB, so a `.place`
    /// was read, validated and dropped without a word. Seventy Doom worlds
    /// published that way: no placements, no error, an empty level for
    /// anything streaming the map.
    #[test]
    fn a_world_publishes_its_place_sidecar() {
        let pack = test_root("placesidecar");
        let out = test_bundle("placesidecar_out");
        fs::create_dir_all(pack.join("worlds")).unwrap();
        fs::write(pack.join("worlds/e1m1.glb"), tiny_glb()).unwrap();
        fs::write(pack.join("worlds/e1m1.png"), valid_png(512, 512)).unwrap();
        fs::write(
            pack.join("worlds/e1m1.place"),
            "world-place 1\nsource doom\nworld e1m1\n\
             place t0 character billboards/doom1/poss 1.0000 0.0000 2.0000 0.00000\n",
        )
        .unwrap();

        let report = compile_pack(&pack, &out, licensed_spec(), None, false).expect("compile");
        assert_eq!(report.assets, 1, "the world is one asset, not two");
        let manifest =
            ImportManifest::from_canonical_bytes(&fs::read(&report.manifest_path).unwrap())
                .unwrap();
        let asset = &manifest.assets[0];
        assert_eq!(asset.key.as_str(), "worlds/e1m1");
        let paths: Vec<&str> = asset.files.iter().map(|f| f.path.as_str()).collect();
        assert!(
            paths.iter().any(|p| p.ends_with(".place")),
            "the placements sidecar must ride on the world's own asset: {paths:?}"
        );
        // Source role, as the lane declares until a typed role can land.
        let place = asset
            .files
            .iter()
            .find(|f| f.path.as_str().ends_with(".place"))
            .expect("place file");
        assert_eq!(place.file.role, FileRole::Source);
    }

    /// An unreadable placements sidecar is left behind and the world still
    /// publishes — the same convenience contract `.spawn` has.
    #[test]
    fn an_unreadable_place_sidecar_does_not_cost_the_world() {
        let pack = test_root("badplace");
        let out = test_bundle("badplace_out");
        fs::create_dir_all(pack.join("worlds")).unwrap();
        fs::write(pack.join("worlds/e1m1.glb"), tiny_glb()).unwrap();
        fs::write(pack.join("worlds/e1m1.png"), valid_png(512, 512)).unwrap();
        fs::write(pack.join("worlds/e1m1.place"), "not a place sidecar at all").unwrap();
        let report = compile_pack(&pack, &out, licensed_spec(), None, false)
            .expect("a bad sidecar must not refuse the world");
        assert_eq!(report.assets, 1);
        let manifest =
            ImportManifest::from_canonical_bytes(&fs::read(&report.manifest_path).unwrap())
                .unwrap();
        let paths: Vec<&str> = manifest.assets[0]
            .files
            .iter()
            .map(|f| f.path.as_str())
            .collect();
        assert!(!paths.iter().any(|p| p.ends_with(".place")), "{paths:?}");
    }

    /// brick-kit ships `square-lq-brick-slope-corner-outside-inverted-2x2`
    /// and its `hq` twin: 49 bytes of stem against the catalog's 48-byte
    /// key-segment budget. That budget is the CONTRACT's
    /// (`makepad_asset_data::limits::MAX_KEY_SEGMENT_BYTES`) and is frozen,
    /// so the two models genuinely cannot publish — but 294 others in the
    /// kit can, and used to be lost with them.
    #[test]
    fn a_name_too_long_to_be_a_catalog_key_costs_that_model_only() {
        let pack = test_root("longname");
        let out = test_bundle("longname_out");
        let long = "square-lq-brick-slope-corner-outside-inverted-2x2";
        assert_eq!(long.len(), 49, "the real brick-kit stem, one byte over");
        assert!(long.parse::<PackEntryKey>().is_err(), "cannot be a key");
        fs::write(pack.join(format!("{long}.glb")), tiny_glb()).unwrap();
        fs::write(pack.join(format!("{long}.png")), valid_png(512, 512)).unwrap();
        for stem in ["brick-a", "brick-b"] {
            fs::write(pack.join(format!("{stem}.glb")), tiny_glb()).unwrap();
            fs::write(pack.join(format!("{stem}.png")), valid_png(512, 512)).unwrap();
        }
        let report = compile_pack(&pack, &out, licensed_spec(), None, false)
            .expect("one over-long name must not refuse the kit");
        assert_eq!(report.assets, 2, "the other models imported");
        assert_eq!(report.skipped_models.len(), 1);
        let (path, why) = &report.skipped_models[0];
        assert_eq!(path, &format!("{long}.glb"));
        assert!(why.contains("cannot exist in the catalog"), "{why}");
        // Its thumbnail did not survive it as a stray image asset.
        let manifest =
            ImportManifest::from_canonical_bytes(&fs::read(&report.manifest_path).unwrap())
                .unwrap();
        let keys: Vec<&str> = manifest.assets.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(keys, ["brick-a", "brick-b"]);
    }

    /// The per-model escape hatch is for shapes we do not SUPPORT, never
    /// for a pack that is broken or trying something. A model pointing its
    /// texture outside the pack refuses the whole pack, even standing
    /// beside models that are perfectly fine — being skippable would turn
    /// a security refusal into a line in a summary nobody reads.
    #[test]
    fn a_malformed_model_still_refuses_the_whole_pack() {
        let pack = test_root("hostile");
        let out = test_bundle("hostile_out");
        fs::write(pack.join("good.glb"), tiny_glb()).unwrap();
        fs::write(pack.join("good.png"), valid_png(512, 512)).unwrap();
        fs::write(
            pack.join("evil.glb"),
            tiny_glb_with_uri("file:///etc/passwd"),
        )
        .unwrap();
        fs::write(pack.join("evil.png"), valid_png(512, 512)).unwrap();
        let err = compile_pack(&pack, &out, licensed_spec(), None, false)
            .expect_err("an external texture uri must refuse the pack, not skip one model");
        assert_eq!(err.kind, PackImportErrorKind::Malformed);
        assert!(err.to_string().contains("external"), "{err}");
    }

    fn tiny_glb_with_two_uris(a: &str, b: &str) -> Vec<u8> {
        let positions: [f32; 9] = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let mut bin: Vec<u8> = Vec::new();
        for f in positions {
            bin.extend_from_slice(&f.to_le_bytes());
        }
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},
            "nodes":[{{"mesh":0}}],
            "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}}}}]}}],
            "accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}}],
            "bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":{}}}],
            "buffers":[{{"byteLength":{}}}],
            "images":[{{"uri":"{a}"}},{{"uri":"{b}"}}]}}"#,
            bin.len(),
            bin.len()
        );
        let mut json_bytes = json.into_bytes();
        while json_bytes.len() % 4 != 0 {
            json_bytes.push(b' ');
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(b"JSON");
        out.extend_from_slice(&json_bytes);
        out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(&bin);
        out
    }

    fn truncated_jpeg_sof_only() -> Vec<u8> {
        let mut v = vec![0xff, 0xd8, 0xff, 0xc0];
        v.extend_from_slice(&11u16.to_be_bytes());
        v.push(8);
        v.extend_from_slice(&8u16.to_be_bytes());
        v.extend_from_slice(&8u16.to_be_bytes());
        v.push(1);
        v.extend_from_slice(&[1, 0x11, 0]);
        v
    }

    fn jpeg_sof_sos_eoi() -> Vec<u8> {
        let mut v = truncated_jpeg_sof_only();
        v.extend_from_slice(&[0xff, 0xda]);
        v.extend_from_slice(&8u16.to_be_bytes());
        v.push(1);
        v.extend_from_slice(&[1, 0x00, 0, 63, 0]);
        v.extend_from_slice(&[0xff, 0xd9]);
        v
    }

    fn hostile_png_bad_idat() -> Vec<u8> {
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&16u32.to_be_bytes());
        ihdr.extend_from_slice(&16u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        push_png_chunk(&mut out, b"IHDR", &ihdr);
        push_png_chunk(&mut out, b"IDAT", &[0x78, 0x01, 0xff]);
        push_png_chunk(&mut out, b"IEND", &[]);
        out
    }

    fn tiny_glb_with_images_and_buffer_uri(images_json: &str, buffer_uri: Option<&str>) -> Vec<u8> {
        let positions: [f32; 9] = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let mut bin: Vec<u8> = Vec::new();
        for f in positions {
            bin.extend_from_slice(&f.to_le_bytes());
        }
        let buf = match buffer_uri {
            Some(uri) => {
                let escaped = uri.replace('\\', "\\\\").replace('"', "\\\"");
                format!(
                    r#"{{"byteLength":{},"uri":"{escaped}"}}"#,
                    bin.len()
                )
            }
            None => format!(r#"{{"byteLength":{}}}"#, bin.len()),
        };
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},
            "nodes":[{{"mesh":0}}],
            "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}}}}]}}],
            "accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}}],
            "bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":{}}}],
            "buffers":[{buf}],
            "images":{images_json}}}"#,
            bin.len()
        );
        let mut json_bytes = json.into_bytes();
        while json_bytes.len() % 4 != 0 {
            json_bytes.push(b' ');
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(b"JSON");
        out.extend_from_slice(&json_bytes);
        out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(&bin);
        out
    }

    #[test]
    fn nested_absent_out_inside_pack_is_refused() {
        let pack = test_root("out_in_pack");
        fs::write(pack.join("ok.png"), valid_png(16, 16)).unwrap();
        let out = pack.join("missing").join("nested").join("bundle");
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Traversal);
        assert!(err.to_string().contains("--out"), "{err}");
        assert!(!out.exists(), "must not create a nested --out under the pack");
    }

    #[cfg(unix)]
    #[test]
    fn fifo_is_refused_before_open() {
        let pack = test_root("fifo_pack");
        let fifo = pack.join("block.png");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo");
        assert!(status.success());
        let err = compile_pack(
            &pack,
            &test_bundle("fifo_out"),
            licensed_spec(),
            None,
            false,
        )
        .unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Special, "{err}");
        assert!(
            err.to_string().contains("special") || err.to_string().contains("block.png"),
            "{err}"
        );
    }

    #[test]
    fn truncated_jpeg_without_sos_eoi_refuses() {
        let pack = test_root("trunc_jpg");
        fs::write(pack.join("x.jpg"), truncated_jpeg_sof_only()).unwrap();
        assert!(jpeg_dims(&truncated_jpeg_sof_only()).is_some());
        let err = compile_pack(
            &pack,
            &test_bundle("trunc_jpg_out"),
            licensed_spec(),
            None,
            false,
        )
        .unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed, "{err}");
        assert!(
            err.to_string().contains("SOS")
                || err.to_string().contains("EOI")
                || err.to_string().contains("jpeg"),
            "{err}"
        );
    }

    #[test]
    fn glb_external_uri_refuses_and_pack_relative_is_attached() {
        let pack = test_root("glb_http");
        fs::write(
            pack.join("hero.glb"),
            tiny_glb_with_uri("https://example.invalid/tex.png"),
        )
        .unwrap();
        fs::write(pack.join("hero.png"), valid_png(256, 256)).unwrap();
        let err = compile_pack(
            &pack,
            &test_bundle("glb_http_out"),
            licensed_spec(),
            None,
            false,
        )
        .unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed, "{err}");
        assert!(
            err.to_string().contains("uri") || err.to_string().contains("external"),
            "{err}"
        );

        let pack = test_root("glb_uri_pack");
        fs::create_dir_all(pack.join("models")).unwrap();
        fs::create_dir_all(pack.join("textures")).unwrap();
        fs::write(
            pack.join("models/box.glb"),
            tiny_glb_with_uri("../textures/atlas.png"),
        )
        .unwrap();
        fs::write(pack.join("models/box.png"), valid_png(256, 256)).unwrap();
        fs::write(pack.join("textures/atlas.png"), valid_png(64, 32)).unwrap();
        let report = compile(&pack, &test_bundle("glb_uri_out"), licensed_spec());
        let manifest =
            ImportManifest::from_canonical_bytes(&fs::read(report.manifest_path).unwrap()).unwrap();
        let mesh = manifest
            .assets
            .iter()
            .find(|a| a.kind == AssetKind::Mesh)
            .unwrap();
        assert!(
            mesh.files.iter().any(|f| {
                f.path == "textures/atlas.png" && f.file.role == FileRole::Texture
            }),
            "{:?}",
            mesh.files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
        let plan = String::from_utf8(fs::read(report.plan_path).unwrap()).unwrap();
        let register = plan.find("\"op\":\"register_source_collection\"").unwrap();
        let upload = plan.find("\"op\":\"upload_blob\"").unwrap();
        let import = plan.find("\"op\":\"run_import\"").unwrap();
        assert!(register < upload && upload < import);
    }

    #[test]
    fn glb_hidden_image1_and_buffer_uri_are_refused() {
        let pack = test_root("glb_img1");
        fs::create_dir_all(pack.join("models")).unwrap();
        fs::create_dir_all(pack.join("textures")).unwrap();
        fs::write(pack.join("textures/atlas.png"), valid_png(32, 32)).unwrap();
        fs::write(pack.join("models/box.png"), valid_png(256, 256)).unwrap();
        let images = r#"[{"uri":"../textures/atlas.png"},{"uri":"https://example.invalid/hidden.png"}]"#;
        fs::write(
            pack.join("models/box.glb"),
            tiny_glb_with_images_and_buffer_uri(images, None),
        )
        .unwrap();
        let err = compile_pack(
            &pack,
            &test_bundle("glb_img1_out"),
            licensed_spec(),
            None,
            false,
        )
        .unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed, "{err}");
        assert!(
            err.to_string().contains("uri")
                || err.to_string().contains("external")
                || err.to_string().contains("multi-texture"),
            "{err}"
        );

        let pack = test_root("glb_bufuri");
        fs::write(pack.join("hero.png"), valid_png(256, 256)).unwrap();
        fs::write(
            pack.join("hero.glb"),
            tiny_glb_with_images_and_buffer_uri(
                "[]",
                Some("https://example.invalid/mesh.bin"),
            ),
        )
        .unwrap();
        let err = compile_pack(
            &pack,
            &test_bundle("glb_bufuri_out"),
            licensed_spec(),
            None,
            false,
        )
        .unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed, "{err}");
        assert!(
            err.to_string().contains("uri")
                || err.to_string().contains("external")
                || err.to_string().contains("buffer"),
            "{err}"
        );
    }

    #[test]
    fn hostile_png_idat_and_empty_jpeg_scan_refuse() {
        assert!(super::validate_png(&hostile_png_bad_idat(), "x.png").is_err());
        let pack = test_root("bad_idat");
        fs::write(pack.join("x.png"), hostile_png_bad_idat()).unwrap();
        let err = compile_pack(
            &pack,
            &test_bundle("bad_idat_out"),
            licensed_spec(),
            None,
            false,
        )
        .unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed, "{err}");

        assert!(jpeg_dims(&jpeg_sof_sos_eoi()).is_some());
        let pack = test_root("empty_scan");
        fs::write(pack.join("x.jpg"), jpeg_sof_sos_eoi()).unwrap();
        let err = compile_pack(
            &pack,
            &test_bundle("empty_scan_out"),
            licensed_spec(),
            None,
            false,
        )
        .unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed, "{err}");
        assert!(
            err.to_string().contains("scan")
                || err.to_string().contains("jpeg")
                || err.to_string().contains("decode"),
            "{err}"
        );
    }

    #[test]
    fn ktx2_textures_measure_from_their_header() {
        let mut ktx = super::KTX2_IDENTIFIER.to_vec();
        ktx.resize(80, 0);
        ktx[20..24].copy_from_slice(&512u32.to_le_bytes());
        ktx[24..28].copy_from_slice(&256u32.to_le_bytes());
        assert_eq!(super::validate_ktx2(&ktx, "x.glb").unwrap(), (512, 256));
        // Truncated, a GPU-format KTX2 (vkFormat set) and a sizeless one are refused.
        assert!(super::validate_ktx2(&ktx[..79], "x.glb").is_err());
        let mut gpu = ktx.clone();
        gpu[12..16].copy_from_slice(&145u32.to_le_bytes());
        assert!(super::validate_ktx2(&gpu, "x.glb").is_err());
        let mut empty = ktx.clone();
        empty[20..24].copy_from_slice(&0u32.to_le_bytes());
        assert!(super::validate_ktx2(&empty, "x.glb").is_err());
    }

    #[test]
    fn late_add_after_enumeration_is_changed() {
        let pack = test_root("late_add");
        fs::write(pack.join("ok.png"), valid_png(16, 16)).unwrap();
        let extra = pack.join("z.png");
        super::install_after_enum_hook(move || {
            fs::write(&extra, valid_png(16, 16)).unwrap();
        });
        let err = compile_pack(
            &pack,
            &test_bundle("late_add_out"),
            licensed_spec(),
            None,
            false,
        )
        .unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Changed, "{err}");
        assert!(err.to_string().contains("directory"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn out_parent_symlink_swap_during_hash_is_refused() {
        let base = test_root("out_swap");
        let pack = base.join("pack");
        let safe = base.join("safe");
        fs::create_dir_all(pack.join("sub")).unwrap();
        fs::create_dir_all(safe.join("link").join("sub")).unwrap();
        fs::write(pack.join("ok.png"), valid_png(16, 16)).unwrap();
        let out = safe.join("link").join("sub").join("out");
        let link = safe.join("link");
        let pack2 = pack.clone();
        super::install_after_enum_hook(move || {
            fs::remove_dir_all(&link).unwrap();
            std::os::unix::fs::symlink(&pack2, &link).unwrap();
        });
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert!(
            err.kind == PackImportErrorKind::Traversal
                || err.kind == PackImportErrorKind::Special,
            "{err}"
        );
        assert!(
            !pack.join("sub").join("out").exists(),
            "must not publish inside the pack after --out parent swap"
        );

        let base = test_root("out_swap_exact");
        let pack = base.join("pack");
        let safe = base.join("safe");
        fs::create_dir_all(pack.join("sub")).unwrap();
        fs::create_dir_all(safe.join("link").join("sub")).unwrap();
        fs::write(pack.join("ok.png"), valid_png(16, 16)).unwrap();
        let out = safe.join("link").join("sub").join("out");
        compile(&pack, &out, licensed_spec());
        let planted_src = out.clone();
        let stash = base.join("stash");
        fs::create_dir_all(&stash).unwrap();
        for name in [
            SOURCE_COLLECTION_FILE,
            IMPORT_MANIFEST_FILE,
            UPLOAD_PLAN_FILE,
        ] {
            fs::copy(planted_src.join(name), stash.join(name)).unwrap();
        }
        let link = safe.join("link");
        let pack2 = pack.clone();
        super::install_after_enum_hook(move || {
            fs::remove_dir_all(&link).unwrap();
            std::os::unix::fs::symlink(&pack2, &link).unwrap();
            fs::rename(&stash, pack2.join("sub").join("out")).unwrap();
        });
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert!(
            err.kind == PackImportErrorKind::Traversal
                || err.kind == PackImportErrorKind::Special
                || err.kind == PackImportErrorKind::Changed,
            "{err}"
        );
        if pack.join("sub").join("out").exists() {
            assert!(
                err.kind != PackImportErrorKind::Io,
                "existing-bundle path after swap must not be treated as a successful no-op: {err}"
            );
        }
    }

    fn glb_wrap(json: &str, bin: &[u8]) -> Vec<u8> {
        let mut json_bytes = json.as_bytes().to_vec();
        while json_bytes.len() % 4 != 0 {
            json_bytes.push(b' ');
        }
        let mut bin = bin.to_vec();
        while bin.len() % 4 != 0 {
            bin.push(0);
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(b"JSON");
        out.extend_from_slice(&json_bytes);
        out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(&bin);
        out
    }

    fn triangle_bin() -> Vec<u8> {
        let positions: [f32; 9] = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let mut bin = Vec::new();
        for f in positions {
            bin.extend_from_slice(&f.to_le_bytes());
        }
        bin
    }

    fn triangle_json(nodes: &str, accessor_count: &str) -> String {
        format!(
            r#"{{"asset":{{"version":"2.0"}},
            "nodes":{nodes},
            "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}}}}]}}],
            "accessors":[{{"bufferView":0,"componentType":5126,"count":{accessor_count},"type":"VEC3"}}],
            "bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":36}}],
            "buffers":[{{"byteLength":36}}]}}"#
        )
    }

    #[test]
    fn glb_preflight_refuses_self_cycle_huge_count_duplicate_chunk_and_trailing() {
        let bin = triangle_bin();
        let ok = glb_wrap(&triangle_json(r#"[{"mesh":0}]"#, "3"), &bin);
        assert!(super::preflight_glb(&ok, "ok.glb").is_ok());
        assert!(super::measure_glb(&ok, "ok.glb").is_ok());

        let cyclic = glb_wrap(
            &triangle_json(r#"[{"mesh":0,"children":[0]}]"#, "3"),
            &bin,
        );
        let err = super::preflight_glb(&cyclic, "cycle.glb").unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed, "{err}");
        assert!(err.to_string().contains("child") || err.to_string().contains("cycle"), "{err}");
        let err = super::measure_glb(&cyclic, "cycle.glb").unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed, "{err}");

        let huge = glb_wrap(&triangle_json(r#"[{"mesh":0}]"#, "5000000"), &bin);
        let err = super::preflight_glb(&huge, "huge.glb").unwrap_err();
        assert!(
            err.kind == PackImportErrorKind::Content || err.kind == PackImportErrorKind::Malformed,
            "{err}"
        );
        assert!(err.to_string().contains("count") || err.to_string().contains("MAX_VERTICES"), "{err}");

        let overflow = glb_wrap(
            &triangle_json(r#"[{"mesh":0}]"#, "6148914691236517205"),
            &bin,
        );
        let err = super::preflight_glb(&overflow, "overflow.glb").unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed, "{err}");
        assert!(
            err.to_string().contains("overflow") || err.to_string().contains("count"),
            "{err}"
        );

        let mut dup = ok.clone();
        let json_len = u32::from_le_bytes(ok[12..16].try_into().unwrap()) as usize;
        let json_body = ok[20..20 + json_len].to_vec();
        dup.extend_from_slice(&(json_len as u32).to_le_bytes());
        dup.extend_from_slice(b"JSON");
        dup.extend_from_slice(&json_body);
        let pad = (4 - (json_len % 4)) % 4;
        dup.extend(std::iter::repeat(b' ').take(pad));
        let new_total = dup.len() as u32;
        dup[8..12].copy_from_slice(&new_total.to_le_bytes());
        let err = super::preflight_glb(&dup, "dup.glb").unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed, "{err}");
        assert!(err.to_string().contains("duplicate"), "{err}");

        let mut trail = ok.clone();
        trail.push(0xff);
        let err = super::preflight_glb(&trail, "trail.glb").unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed, "{err}");
        assert!(
            err.to_string().contains("trailing") || err.to_string().contains("length"),
            "{err}"
        );
    }

    // -----------------------------------------------------------------
    // One asset per actor / textures ride with their mesh
    // -----------------------------------------------------------------

    fn write_billboard_actor(dir: &Path) {
        // The shape classic import writes: ONE packed sheet, ONE manifest
        // indexing its cells, one animated preview strip.
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("troo.png"), valid_png(8, 6)).unwrap();
        fs::write(dir.join("troo_thumb.png"), valid_png(1024, 256)).unwrap();
        fs::write(
            dir.join("troo.billboard"),
            "stateful-billboard 1\n\
             prefix troo\n\
             role character\n\
             preview walk\n\
             facings 1\n\
             sheet 2 4 6\n\
             state walk 0 2 1 8\n\
             frame 0 A 1 4 6 troo.png cell 0\n\
             frame 1 B 1 4 6 troo.png flip cell 1\n",
        )
        .unwrap();
    }

    #[test]
    fn billboard_actor_is_one_asset_with_its_sheet_and_manifest() {
        let pack = test_root("bb_actor");
        let out = test_bundle("bb_actor_out");
        write_billboard_actor(&pack.join("billboards/doom1"));
        let report = compile(&pack, &out, licensed_spec());
        assert_eq!(report.assets, 1, "one actor, one card");
        let manifest =
            ImportManifest::from_canonical_bytes(&fs::read(&report.manifest_path).unwrap()).unwrap();
        let asset = &manifest.assets[0];
        assert_eq!(asset.kind, AssetKind::Billboard);
        assert_eq!(asset.key.as_str(), "billboards/doom1/troo");
        let roles: Vec<(FileRole, &str)> = asset
            .files
            .iter()
            .map(|f| (f.file.role, f.path.as_str()))
            .collect();
        assert_eq!(
            roles,
            vec![
                (FileRole::Texture, "billboards/doom1/troo.png"),
                (FileRole::Source, "billboards/doom1/troo.billboard"),
            ]
        );
        assert_eq!(
            asset.files[0].file.dims,
            Some(ImageDims { width: 8, height: 6 }),
            "the sheet publishes its dimensions"
        );
        assert_eq!(asset.files[1].file.media, MediaType::Text);
        let thumb = asset.thumbnail.as_ref().expect("animated preview strip");
        assert_eq!(thumb.path, "billboards/doom1/troo_thumb.png");
        assert_eq!((thumb.meta.width, thumb.meta.height), (1024, 256));
    }

    #[test]
    fn per_frame_pngs_of_an_actor_never_become_assets() {
        let pack = test_root("bb_frames");
        let out = test_bundle("bb_frames_out");
        let dir = pack.join("billboards/doom1");
        write_billboard_actor(&dir);
        // A legacy actor beside it: per-frame PNGs, no packed sheet. It
        // publishes NOTHING rather than one card per lump.
        for lump in ["bossa1", "bossa2a8", "bossb1"] {
            fs::write(dir.join(format!("{lump}.png")), valid_png(4, 6)).unwrap();
        }
        fs::write(
            dir.join("boss.billboard"),
            "stateful-billboard 1\n\
             prefix boss\n\
             role character\n\
             preview walk\n\
             facings 8\n\
             mirrors 8\n\
             state walk 0 3 1 8\n\
             frame 0 A 1 4 6 bossa1.png\n\
             frame 1 A 2 4 6 bossa2a8.png\n\
             frame 2 A 8 4 6 bossa2a8.png flip\n\
             frame 3 B 1 4 6 bossb1.png\n",
        )
        .unwrap();
        let report = compile(&pack, &out, licensed_spec());
        let manifest =
            ImportManifest::from_canonical_bytes(&fs::read(&report.manifest_path).unwrap()).unwrap();
        let keys: Vec<&str> = manifest.assets.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(keys, vec!["billboards/doom1/troo"], "{keys:?}");
        let plan = String::from_utf8(fs::read(&report.plan_path).unwrap()).unwrap();
        assert!(
            !plan.contains("bossa2a8.png"),
            "frame pixels are not uploaded at all"
        );
    }

    #[test]
    fn a_manifest_that_lies_about_its_sheet_is_refused() {
        let pack = test_root("bb_lies");
        let out = test_bundle("bb_lies_out");
        let dir = pack.join("billboards/doom1");
        write_billboard_actor(&dir);
        // Header claims 2x4 wide cells; the sheet on disk is 8x6.
        fs::write(
            dir.join("troo.billboard"),
            "stateful-billboard 1\n\
             prefix troo\n\
             role character\n\
             preview walk\n\
             sheet 2 9 6\n\
             state walk 0 2 1 8\n\
             frame 0 A 1 4 6 troo.png cell 0\n\
             frame 1 B 1 4 6 troo.png cell 1\n",
        )
        .unwrap();
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Malformed, "{err}");
        assert!(err.to_string().contains("sheet header"), "{err}");
    }

    #[test]
    fn a_mesh_packs_atlas_is_attached_never_an_orphan_texture() {
        let pack = test_root("atlas");
        let out = test_bundle("atlas_out");
        fs::create_dir_all(pack.join("models")).unwrap();
        fs::create_dir_all(pack.join("Textures")).unwrap();
        fs::write(pack.join("models/box.glb"), tiny_glb()).unwrap();
        fs::write(pack.join("models/box.png"), valid_png(512, 512)).unwrap();
        fs::write(pack.join("Textures/colormap.png"), valid_png(1024, 1024)).unwrap();
        let report = compile(&pack, &out, licensed_spec());
        let manifest =
            ImportManifest::from_canonical_bytes(&fs::read(&report.manifest_path).unwrap()).unwrap();
        assert_eq!(report.assets, 1, "the kit is one mesh, not mesh + atlas");
        assert_eq!(manifest.assets[0].kind, AssetKind::Mesh);
        assert!(
            !manifest.assets.iter().any(|a| a.kind == AssetKind::Texture),
            "a deletable texture row is a dependency waiting to break"
        );
    }

    #[test]
    fn an_image_only_pack_still_publishes_its_textures() {
        let pack = test_root("tex_only");
        let out = test_bundle("tex_only_out");
        fs::create_dir_all(pack.join("textures")).unwrap();
        fs::write(pack.join("textures/hull.png"), valid_png(512, 256)).unwrap();
        fs::write(pack.join("textures/detail.jpg"), jpeg_solid(64, 48)).unwrap();
        let report = compile(&pack, &out, licensed_spec());
        let manifest =
            ImportManifest::from_canonical_bytes(&fs::read(&report.manifest_path).unwrap()).unwrap();
        assert_eq!(report.assets, 2, "a ui/sprite pack is its images");
        assert!(manifest.assets.iter().all(|a| a.kind == AssetKind::Texture));
    }


    #[test]
    fn a_calibrated_kenney_pack_publishes_metric_dimensions() {
        // space-kit is calibrated at 2.2 m per native unit (its astronaut
        // pin), so a mesh with the astronaut's 0.795-unit height publishes
        // the shared 1.75 m person in metres — the law every importer pins
        // to, so a Kenney astronaut and a Doom imp mash up at one scale.
        let pack = test_root("kscale");
        let out = test_bundle("kscale_out");
        fs::create_dir_all(pack.join("models")).unwrap();
        fs::write(
            pack.join("models/astronaut.glb"),
            glb_with_positions([0.0, 0.0, 0.0, 0.48, 0.0, 0.0, 0.0, 0.795, 0.33]),
        )
        .unwrap();
        fs::write(pack.join("models/astronaut.png"), valid_png(512, 512)).unwrap();
        let report = compile(&pack, &out, licensed_spec());
        let manifest =
            ImportManifest::from_canonical_bytes(&fs::read(&report.manifest_path).unwrap()).unwrap();
        let mesh = &manifest.assets[0];
        // The coordinate system tells the truth: 2.2 metres per native unit.
        assert!(
            (mesh.coordinate_system.units_per_meter - 1.0 / 2.2).abs() < 1e-4,
            "{}",
            mesh.coordinate_system.units_per_meter
        );
        let find = |name: &str| {
            mesh.anchors
                .iter()
                .find(|a| a.name == name)
                .unwrap_or_else(|| panic!("{name} missing from {:?}", mesh.anchors))
        };
        assert!(
            (find("dim_height").transform.pos.y - 1.75).abs() < 0.01,
            "an astronaut-sized mesh is a 1.75 m person: {}",
            find("dim_height").transform.pos.y
        );
        assert!((find("scale_real").transform.scale.y - 2.2).abs() < 1e-4);
        assert!((find("scale_comic").transform.scale.y - 1.0).abs() < 1e-6);
        assert!((find("scale_small").transform.scale.y - 0.4 / 0.795).abs() < 1e-4);
        assert!((find("scale_handheld").transform.scale.y - 0.25 / 0.795).abs() < 1e-4);
    }

    #[test]
    fn an_uncatalogued_pack_is_taken_at_its_word_but_still_states_its_size() {
        let pack = test_root("noscale");
        let out = test_bundle("noscale_out");
        fs::create_dir_all(pack.join("models")).unwrap();
        // A door-sized mesh in a pack the calibration table does not know:
        // its authored units are taken as metres, and the manifest still
        // carries the measured dimensions and presets.
        fs::write(
            pack.join("models/door.glb"),
            glb_with_positions([0.0, 0.0, 0.0, 0.9, 0.0, 0.0, 0.0, 2.1, 0.1]),
        )
        .unwrap();
        fs::write(pack.join("models/door.png"), valid_png(512, 512)).unwrap();
        let mut spec = licensed_spec();
        spec.pack_name = Some("mystery-kit".into());
        assert!(pack_scale("mystery-kit").is_none(), "the table must not know it");
        let report = compile(&pack, &out, spec);
        let manifest =
            ImportManifest::from_canonical_bytes(&fs::read(&report.manifest_path).unwrap()).unwrap();
        let mesh = &manifest.assets[0];
        assert_eq!(mesh.coordinate_system.units_per_meter, 1.0);
        let find = |name: &str| {
            mesh.anchors
                .iter()
                .find(|a| a.name == name)
                .unwrap_or_else(|| panic!("{name} missing from {:?}", mesh.anchors))
        };
        assert!((find("dim_height").transform.pos.y - 2.1).abs() < 1e-4);
        assert!((find("scale_real").transform.scale.y - 1.0).abs() < 1e-6);
        // Small and handheld shrink by the largest extent (2.1 m).
        assert!((find("scale_small").transform.scale.y - 0.4 / 2.1).abs() < 1e-4);
    }

    #[test]
    fn a_worlds_spawn_sidecar_publishes_as_anchors_and_never_as_a_blob() {
        let pack = test_root("nav");
        let out = test_bundle("nav_out");
        fs::create_dir_all(pack.join("worlds")).unwrap();
        fs::write(pack.join("worlds/map01.glb"), tiny_glb()).unwrap();
        fs::write(pack.join("worlds/map01.png"), valid_png(512, 512)).unwrap();
        fs::write(
            pack.join("worlds/map01.spawn"),
            "world-spawn 1\n1.0000 0.6406 1.0000\n1.57080 0.00000\n\
             start player_start 1.0000 0.6406 1.0000 1.57080 0.00000\n\
             start deathmatch_1 4.0000 0.6406 -2.0000 -1.57080 0.00000\n\
             floor 0.0000\nstep 0.3750\neye 0.6406\n",
        )
        .unwrap();
        let report = compile(&pack, &out, licensed_spec());
        assert_eq!(report.assets, 1, "the sidecar is metadata, not an asset");
        let manifest =
            ImportManifest::from_canonical_bytes(&fs::read(&report.manifest_path).unwrap()).unwrap();
        let world = &manifest.assets[0];
        let names: Vec<&str> = world.anchors.iter().map(|a| a.name.as_str()).collect();
        // canonicalize() sorts anchors by name.
        assert_eq!(
            names,
            vec![
                "deathmatch_1",
                "eye_height",
                "floor_height",
                "player_start",
                "step_height"
            ]
        );
        let ps = world.anchors.iter().find(|a| a.name == "player_start").unwrap();
        assert!((ps.transform.pos.y - 0.6406).abs() < 1e-4);
        assert!((ps.transform.rot.y - std::f32::consts::FRAC_PI_4.sin()).abs() < 1e-4);
        let step = world.anchors.iter().find(|a| a.name == "step_height").unwrap();
        assert!((step.transform.pos.y - 0.375).abs() < 1e-4);

        let plan = String::from_utf8(fs::read(&report.plan_path).unwrap()).unwrap();
        assert!(!plan.contains("map01.spawn"), "sidecar must not be uploaded");
        assert!(
            !world.files.iter().any(|f| f.path.ends_with(".spawn")),
            "no file role carries navigation"
        );
    }

    #[test]
    fn a_world_without_a_sidecar_still_publishes() {
        let pack = test_root("nonav");
        let out = test_bundle("nonav_out");
        fs::create_dir_all(pack.join("worlds")).unwrap();
        fs::write(pack.join("worlds/map02.glb"), tiny_glb()).unwrap();
        fs::write(pack.join("worlds/map02.png"), valid_png(512, 512)).unwrap();
        let report = compile(&pack, &out, licensed_spec());
        let manifest =
            ImportManifest::from_canonical_bytes(&fs::read(&report.manifest_path).unwrap()).unwrap();
        assert!(manifest.assets[0].anchors.is_empty());
    }

    #[test]
    fn a_placeholder_thumbnail_refuses_the_mesh() {
        let pack = test_root("placeholder");
        let out = test_bundle("placeholder_out");
        fs::create_dir_all(pack.join("models")).unwrap();
        fs::write(pack.join("models/box.glb"), tiny_glb()).unwrap();
        // A flat 512² tile: the "no visual available" shape.
        let mut rgba = Vec::new();
        for _ in 0..512 * 512 {
            rgba.extend_from_slice(&[32, 38, 46, 255]);
        }
        let flat = crate::png::encode_png_rgba(&rgba, 512, 512).unwrap();
        fs::write(pack.join("models/box.png"), flat).unwrap();
        let err = compile_pack(&pack, &out, licensed_spec(), None, false).unwrap_err();
        assert_eq!(err.kind, PackImportErrorKind::Content, "{err}");
        assert!(err.to_string().contains("placeholder"), "{err}");
        assert!(err.to_string().contains("models/box.png"), "{err}");
    }

    #[test]
    fn atlas_directory_is_recognized_at_any_depth() {
        assert!(is_pack_atlas("textures/colormap.png"));
        assert!(is_pack_atlas("kit/Textures/colormap.png"));
        assert!(is_pack_atlas("a/b/TEXTURES/c/colormap.png"));
        assert!(!is_pack_atlas("models/box.png"));
        assert!(!is_pack_atlas("textures.png"));
    }
}
