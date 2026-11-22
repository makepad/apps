//! The cloud layers' data model (design §1.1): `SkyParams::cloud_stack`, its layers, the five phase-1 type blocks
//! and the marker the lift leaves on an older layer, with their ranges, defaults, clamp, names and seeds. Pure params
//! code: nothing here draws.
//!
//! - Every range lives once, in a `CLOUD_*` const that clamp reads; the inspector's sliders and the assistant's
//!   schema read the same consts (D17).
//! - A block's own fields are required and the block itself is optional: a layer reads the block of its genus, or
//!   that type's defaults when it has none (`cirrus_params()` and its kin), and keeps every other block, so a Type
//!   change and back finds its numbers again.
//! - A genus this build does not know is kept as written and not drawn; clamp never rewrites it.
//! - A child of `params` (its `#[path]` line), so it shares clamp's `Fixes`, `is_choice`, `join_path`,
//!   `LIGHT_NAME_LIMIT` and `clamped_copy!`.

use super::*;
use crate::noise::{hash01, hash_u32};

/// `CloudLayerParams::genus`: the twelve types in the cloud chart's order, as files store them.
pub const CLOUD_GENUS_NAMES: [&str; 12] = [
    "cirrus",
    "cirrocumulus",
    "cirrostratus",
    "altocumulus",
    "altostratus",
    "nimbostratus",
    "stratus",
    "stratocumulus",
    "cumulus",
    "cumulonimbus",
    "lenticular",
    "mammatus",
];

/// Typed view of `CloudLayerParams::genus`, in CLOUD_GENUS_NAMES' (the chart's) order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CloudGenus {
    Cirrus,
    Cirrocumulus,
    Cirrostratus,
    Altocumulus,
    Altostratus,
    Nimbostratus,
    Stratus,
    Stratocumulus,
    Cumulus,
    Cumulonimbus,
    Lenticular,
    Mammatus,
}

/// Phase 1's types, in chart order: the ones drawn, the Type dropdown's rows and the ones set_clouds accepts.
pub const CLOUD_GENUS_BUILT: [CloudGenus; 9] = [
    CloudGenus::Cirrus,
    CloudGenus::Cirrostratus,
    CloudGenus::Altocumulus,
    CloudGenus::Nimbostratus,
    CloudGenus::Stratus,
    CloudGenus::Stratocumulus,
    CloudGenus::Cumulus,
    CloudGenus::Cumulonimbus,
    CloudGenus::Lenticular,
];

/// Each type's salt in a layer's streams (`fresh_cloud_seed`, `cloud_layer_seed`): [g] = 0x85EB_CA6B * (g + 1),
/// wrapping. Never changed: every file's look rests on it.
pub const CLOUD_GENUS_SALT: [u32; 12] = {
    let mut salt = [0u32; 12];
    let mut g = 0;
    while g < 12 {
        salt[g] = 0x85EB_CA6Bu32.wrapping_mul(g as u32 + 1);
        g += 1;
    }
    salt
};

impl CloudGenus {
    /// Every type, in chart order: `genus as usize` indexes it, CLOUD_GENUS_NAMES and CLOUD_GENUS_SALT.
    pub const ALL: [CloudGenus; 12] = [
        CloudGenus::Cirrus,
        CloudGenus::Cirrocumulus,
        CloudGenus::Cirrostratus,
        CloudGenus::Altocumulus,
        CloudGenus::Altostratus,
        CloudGenus::Nimbostratus,
        CloudGenus::Stratus,
        CloudGenus::Stratocumulus,
        CloudGenus::Cumulus,
        CloudGenus::Cumulonimbus,
        CloudGenus::Lenticular,
        CloudGenus::Mammatus,
    ];

    /// The string stored in `CloudLayerParams::genus` ("cirrus").
    pub fn as_str(self) -> &'static str {
        CLOUD_GENUS_NAMES[self as usize]
    }

    /// The name rows, titles and the Type dropdown show ("Cirrus").
    pub fn label(self) -> &'static str {
        match self {
            CloudGenus::Cirrus => "Cirrus",
            CloudGenus::Cirrocumulus => "Cirrocumulus",
            CloudGenus::Cirrostratus => "Cirrostratus",
            CloudGenus::Altocumulus => "Altocumulus",
            CloudGenus::Altostratus => "Altostratus",
            CloudGenus::Nimbostratus => "Nimbostratus",
            CloudGenus::Stratus => "Stratus",
            CloudGenus::Stratocumulus => "Stratocumulus",
            CloudGenus::Cumulus => "Cumulus",
            CloudGenus::Cumulonimbus => "Cumulonimbus",
            CloudGenus::Lenticular => "Lenticular",
            CloudGenus::Mammatus => "Mammatus",
        }
    }

    /// The type a stored genus names, trimmed and in any case; None for a name this build does not know.
    pub fn of(name: &str) -> Option<CloudGenus> {
        CloudGenus::ALL.into_iter().find(|genus| is_choice(name, genus.as_str()))
    }

    /// Drawn by this build, offered by the Type dropdown and accepted by set_clouds (CLOUD_GENUS_BUILT).
    pub fn is_built(self) -> bool {
        CLOUD_GENUS_BUILT.contains(&self)
    }

    /// A layer's Cover when it has none: the share of sky the type hides by default (design §4.3). A saved layer with
    /// no Cover draws at it, so it never changes; what a new layer is made with is `new_cover`.
    pub fn default_cover(self) -> f32 {
        match self {
            CloudGenus::Cirrus | CloudGenus::Cumulus => 0.35,
            CloudGenus::Cirrocumulus => 0.4,
            CloudGenus::Cirrostratus => 0.9,
            CloudGenus::Altostratus => 0.55,
            CloudGenus::Altocumulus | CloudGenus::Stratus | CloudGenus::Stratocumulus | CloudGenus::Mammatus => 0.6,
            CloudGenus::Nimbostratus | CloudGenus::Cumulonimbus | CloudGenus::Lenticular => 1.0,
        }
    }

    /// The Cover a new layer of the type is made with ("+ Cloud", set_clouds' add, a Type change into it, a weather
    /// pick before its own numbers): its first species' Cover where that is not `default_cover`, else None (the
    /// default). A new Stratus is made closed, its first species nebulosus (the owner, 2026-11-01), while a saved
    /// Stratus with no Cover keeps drawing the 0.6 it was saved with.
    pub fn new_cover(self) -> Option<f32> {
        let first = crate::cloud_species::cloud_species(self).first()?;
        let cover = first.rows.iter().find(|(key, _)| *key == "cover").map(|&(_, value)| value)?;
        (cover != self.default_cover()).then_some(cover)
    }

    /// A layer's Silver lining when it has none, by its light family (design §2.3): ice 0.5, slab 1, heap and the
    /// placed and lens types 0.75; a Stratus volume 0.6 (the volumes design §2.4: a layer's lower forward share; at 1
    /// its veil scattered next to nothing away from the sun and read dark where it is thin).
    pub fn default_lining(self) -> f32 {
        match self {
            CloudGenus::Cirrus | CloudGenus::Cirrocumulus => 0.5,
            CloudGenus::Stratus => 0.6,
            CloudGenus::Cirrostratus | CloudGenus::Altostratus | CloudGenus::Nimbostratus => 1.0,
            CloudGenus::Altocumulus | CloudGenus::Stratocumulus | CloudGenus::Cumulus | CloudGenus::Mammatus | CloudGenus::Cumulonimbus | CloudGenus::Lenticular => 0.75,
        }
    }

    /// A layer's Shade below when it has none (design §4.1): 1, a Cirrus 0.5.
    pub fn default_shade_below(self) -> f32 {
        if self == CloudGenus::Cirrus {
            0.5
        } else {
            1.0
        }
    }
}

/// The largest seed clamp() keeps (`CloudLayerParams::seed`). Dice and fresh seeds draw 1..=CLOUD_SEED_MAX; 0 is a
/// lifted layer's: the map's own streams.
pub const CLOUD_SEED_MAX: u32 = 9_999_999;
/// The Clouds entry's Cover (`CloudStackParams::cover`) and a layer's (`CloudLayerParams::cover`).
pub const CLOUD_COVER_RANGE: (f32, f32) = (0.0, 1.0);
/// `CloudStackParams::march_steps`: every marched layer's step budget toward the horizon.
pub const CLOUD_MARCH_STEPS_RANGE: (u32, u32) = (16, 96);
/// `CloudLayerParams::drift_km`: how far the pattern moves downwind, km.
pub const CLOUD_DRIFT_RANGE: (f32, f32) = (-50.0, 50.0);
/// `CloudLayerParams::brightness`.
pub const CLOUD_BRIGHTNESS_RANGE: (f32, f32) = (0.0, 2.0);
/// `CloudLayerParams::sky_light`.
pub const CLOUD_SKY_LIGHT_RANGE: (f32, f32) = (0.0, 2.0);
/// `CloudLayerParams::lining`: the forward lobe's share, the silver lining.
pub const CLOUD_LINING_RANGE: (f32, f32) = (0.0, 1.0);
/// `CloudLayerParams::shade_below`: how much the layer shades the layers under it.
pub const CLOUD_SHADE_BELOW_RANGE: (f32, f32) = (0.0, 1.0);
/// A Volume's Density (Nimbostratus, Stratocumulus, Cumulus): the march's extinction, 1 a fair-weather cumulus.
pub const CLOUD_DENSITY_RANGE: (f32, f32) = (0.0, 4.0);
/// clamp() keeps a block's `tops_m` at least this far over its `base_m`.
pub const CLOUD_MIN_THICKNESS_M: f32 = 200.0;
/// `ClassicLook::light_m`: the height whose sun lit an older layer (the deck's altitude, the volume's middle).
pub const CLOUD_CLASSIC_LIGHT_RANGE: (f32, f32) = (0.0, 18_000.0);
/// Cirrus: `base_m`, metres.
pub const CLOUD_CIRRUS_BASE_RANGE: (f32, f32) = (3000.0, 18_000.0);
/// Cirrus: `size_km` (a logarithmic slider).
pub const CLOUD_CIRRUS_SIZE_RANGE: (f32, f32) = (0.6, 60.0);
/// Cirrus: `streak_length`.
pub const CLOUD_CIRRUS_STREAK_LENGTH_RANGE: (f32, f32) = (1.0, 30.0);
/// Cirrus: `fineness`.
pub const CLOUD_CIRRUS_FINENESS_RANGE: (f32, f32) = (0.0, 1.0);
/// Cirrus: `hooks`.
pub const CLOUD_CIRRUS_HOOKS_RANGE: (f32, f32) = (0.0, 1.0);
/// Cirrus: `fall_streaks`.
pub const CLOUD_CIRRUS_FALL_STREAKS_RANGE: (f32, f32) = (0.0, 1.0);
/// Cirrus: `patchiness`.
pub const CLOUD_CIRRUS_PATCHINESS_RANGE: (f32, f32) = (0.0, 1.0);
/// Cirrus: `opacity`.
pub const CLOUD_CIRRUS_OPACITY_RANGE: (f32, f32) = (0.0, 1.0);
/// Cirrostratus: `base_m`, metres (a high veil).
pub const CLOUD_CIRROSTRATUS_BASE_RANGE: (f32, f32) = (5000.0, 13_000.0);
/// Cirrostratus: `size_km` (a logarithmic slider): how large its mottles are.
pub const CLOUD_CIRROSTRATUS_SIZE_RANGE: (f32, f32) = (0.5, 10.0);
/// Cirrostratus: `milkiness`.
pub const CLOUD_CIRROSTRATUS_MILKINESS_RANGE: (f32, f32) = (0.0, 1.0);
/// Cirrostratus: `mottling`.
pub const CLOUD_CIRROSTRATUS_MOTTLING_RANGE: (f32, f32) = (0.0, 1.0);
/// Cirrostratus: `fibres`.
pub const CLOUD_CIRROSTRATUS_FIBRES_RANGE: (f32, f32) = (0.0, 1.0);
/// Cirrostratus: `halo`.
pub const CLOUD_CIRROSTRATUS_HALO_RANGE: (f32, f32) = (0.0, 1.0);
/// Altocumulus: `base_m`, metres (a mid-level deck).
pub const CLOUD_ALTOCUMULUS_BASE_RANGE: (f32, f32) = (2000.0, 6000.0);
/// Altocumulus: `cell_m`, metres (a logarithmic slider): how far apart its cloudlets stand.
pub const CLOUD_ALTOCUMULUS_CELL_RANGE: (f32, f32) = (200.0, 3000.0);
/// Altocumulus: `regularity`.
pub const CLOUD_ALTOCUMULUS_REGULARITY_RANGE: (f32, f32) = (0.0, 1.0);
/// Altocumulus: `rows`.
pub const CLOUD_ALTOCUMULUS_ROWS_RANGE: (f32, f32) = (0.0, 1.0);
/// Altocumulus: `turrets`.
pub const CLOUD_ALTOCUMULUS_TURRETS_RANGE: (f32, f32) = (0.0, 1.0);
/// Altocumulus: `softness`.
pub const CLOUD_ALTOCUMULUS_SOFTNESS_RANGE: (f32, f32) = (0.0, 1.0);
/// Nimbostratus: `base_m`, metres.
pub const CLOUD_NIMBOSTRATUS_BASE_RANGE: (f32, f32) = (100.0, 12_000.0);
/// Nimbostratus: `tops_m`, metres, from its lowest base plus CLOUD_MIN_THICKNESS_M.
pub const CLOUD_NIMBOSTRATUS_TOPS_RANGE: (f32, f32) = (300.0, 12_000.0);
/// Nimbostratus: `size_km`.
pub const CLOUD_NIMBOSTRATUS_SIZE_RANGE: (f32, f32) = (0.3, 30.0);
/// Nimbostratus: `darkness`.
pub const CLOUD_NIMBOSTRATUS_DARKNESS_RANGE: (f32, f32) = (0.0, 1.0);
/// Nimbostratus: `unevenness`.
pub const CLOUD_NIMBOSTRATUS_UNEVENNESS_RANGE: (f32, f32) = (0.0, 1.0);
/// Nimbostratus: `ragged_base`.
pub const CLOUD_NIMBOSTRATUS_RAGGED_BASE_RANGE: (f32, f32) = (0.0, 1.0);
/// Nimbostratus: `scud`.
pub const CLOUD_NIMBOSTRATUS_SCUD_RANGE: (f32, f32) = (0.0, 1.0);
/// Nimbostratus: `scud_height_m`, metres.
pub const CLOUD_NIMBOSTRATUS_SCUD_HEIGHT_RANGE: (f32, f32) = (150.0, 1500.0);
/// Nimbostratus: `rain_curtains`.
pub const CLOUD_NIMBOSTRATUS_RAIN_CURTAINS_RANGE: (f32, f32) = (0.0, 1.0);
/// Stratus: `base_m`, metres (to 12000, so a lifted fog deck keeps its altitude).
pub const CLOUD_STRATUS_BASE_RANGE: (f32, f32) = (50.0, 12_000.0);
/// Stratus: `size_km`.
pub const CLOUD_STRATUS_SIZE_RANGE: (f32, f32) = (0.3, 30.0);
/// Stratus: the slab's own `density`, not a Volume's.
pub const CLOUD_STRATUS_DENSITY_RANGE: (f32, f32) = (0.0, 1.0);
/// Stratus: `mottling`.
pub const CLOUD_STRATUS_MOTTLING_RANGE: (f32, f32) = (0.0, 1.0);
/// Stratus: `streaks`.
pub const CLOUD_STRATUS_STREAKS_RANGE: (f32, f32) = (0.0, 1.0);
/// Stratus: `ragged_edges`.
pub const CLOUD_STRATUS_RAGGED_EDGES_RANGE: (f32, f32) = (0.0, 1.0);
/// Stratocumulus: `base_m`, metres.
pub const CLOUD_STRATOCUMULUS_BASE_RANGE: (f32, f32) = (100.0, 12_000.0);
/// Stratocumulus: `tops_m`, metres, from its lowest base plus CLOUD_MIN_THICKNESS_M.
pub const CLOUD_STRATOCUMULUS_TOPS_RANGE: (f32, f32) = (300.0, 12_000.0);
/// Stratocumulus: `size_km`.
pub const CLOUD_STRATOCUMULUS_SIZE_RANGE: (f32, f32) = (0.3, 30.0);
/// Stratocumulus: `softness`.
pub const CLOUD_STRATOCUMULUS_SOFTNESS_RANGE: (f32, f32) = (0.0, 1.0);
/// Stratocumulus: `shading`.
pub const CLOUD_STRATOCUMULUS_SHADING_RANGE: (f32, f32) = (0.0, 1.0);
/// Stratocumulus: `billows`.
pub const CLOUD_STRATOCUMULUS_BILLOWS_RANGE: (f32, f32) = (0.0, 1.0);
/// Stratocumulus: `rolls`.
pub const CLOUD_STRATOCUMULUS_ROLLS_RANGE: (f32, f32) = (0.0, 1.0);
/// Cumulus: `base_m`, metres.
pub const CLOUD_CUMULUS_BASE_RANGE: (f32, f32) = (200.0, 8000.0);
/// Cumulus: `tops_m`, metres, from its lowest base plus CLOUD_MIN_THICKNESS_M.
pub const CLOUD_CUMULUS_TOPS_RANGE: (f32, f32) = (400.0, 12_000.0);
/// Cumulus: `size_km`.
pub const CLOUD_CUMULUS_SIZE_RANGE: (f32, f32) = (0.3, 30.0);
/// Cumulus: `puffiness`.
pub const CLOUD_CUMULUS_PUFFINESS_RANGE: (f32, f32) = (0.0, 1.0);
/// Cumulus: `flat_base`.
pub const CLOUD_CUMULUS_FLAT_BASE_RANGE: (f32, f32) = (0.0, 1.0);
/// Cumulus: `towers`.
pub const CLOUD_CUMULUS_TOWERS_RANGE: (f32, f32) = (0.0, 1.0);
/// Cumulus: `clustering`.
pub const CLOUD_CUMULUS_CLUSTERING_RANGE: (f32, f32) = (0.0, 1.0);
/// Cumulus: `streets`.
pub const CLOUD_CUMULUS_STREETS_RANGE: (f32, f32) = (0.0, 1.0);
/// Cumulus: `lean`.
pub const CLOUD_CUMULUS_LEAN_RANGE: (f32, f32) = (0.0, 1.0);
/// Cumulus: `softness`.
pub const CLOUD_CUMULUS_SOFTNESS_RANGE: (f32, f32) = (0.0, 1.0);
/// Cumulonimbus: `base_m`, metres.
pub const CLOUD_CUMULONIMBUS_BASE_RANGE: (f32, f32) = (300.0, 3000.0);
/// Cumulonimbus: `tops_m`, metres: the anvil's top.
pub const CLOUD_CUMULONIMBUS_TOPS_RANGE: (f32, f32) = (6000.0, 16_000.0);
/// Cumulonimbus: `distance_km`, from the viewer to the tower's middle.
pub const CLOUD_CUMULONIMBUS_DISTANCE_RANGE: (f32, f32) = (5.0, 120.0);
/// Cumulonimbus: `tower_km`, the tower's width (a logarithmic slider).
pub const CLOUD_CUMULONIMBUS_TOWER_RANGE: (f32, f32) = (2.0, 30.0);
/// Cumulonimbus: `anvil`.
pub const CLOUD_CUMULONIMBUS_ANVIL_RANGE: (f32, f32) = (0.0, 1.0);
/// Cumulonimbus: `anvil_m`, the anvil's thickness, metres.
pub const CLOUD_CUMULONIMBUS_ANVIL_THICKNESS_RANGE: (f32, f32) = (300.0, 3000.0);
/// Cumulonimbus: `overshoot`.
pub const CLOUD_CUMULONIMBUS_OVERSHOOT_RANGE: (f32, f32) = (0.0, 1.0);
/// Cumulonimbus: `lean_deg`, the tower's tilt downwind (below 0 upwind).
pub const CLOUD_CUMULONIMBUS_LEAN_RANGE: (f32, f32) = (-30.0, 30.0);
/// Cumulonimbus: `puffiness`.
pub const CLOUD_CUMULONIMBUS_PUFFINESS_RANGE: (f32, f32) = (0.0, 1.0);
/// Cumulonimbus: `darkness`.
pub const CLOUD_CUMULONIMBUS_DARKNESS_RANGE: (f32, f32) = (0.0, 1.0);
/// Cumulonimbus: `rain_shaft`.
pub const CLOUD_CUMULONIMBUS_RAIN_SHAFT_RANGE: (f32, f32) = (0.0, 1.0);
/// Cumulonimbus: `mammatus`.
pub const CLOUD_CUMULONIMBUS_MAMMATUS_RANGE: (f32, f32) = (0.0, 1.0);
/// Lenticular: `base_m`, the lowest plate's underside, metres.
pub const CLOUD_LENTICULAR_BASE_RANGE: (f32, f32) = (2000.0, 12_000.0);
/// Lenticular: `distance_km`, from the viewer to the stack's middle.
pub const CLOUD_LENTICULAR_DISTANCE_RANGE: (f32, f32) = (2.0, 80.0);
/// Lenticular: `length_km`, the plates' length across the wind (a logarithmic slider).
pub const CLOUD_LENTICULAR_LENGTH_RANGE: (f32, f32) = (1.0, 30.0);
/// Lenticular: `width_km`, the plates' width along the wind (a logarithmic slider).
pub const CLOUD_LENTICULAR_WIDTH_RANGE: (f32, f32) = (1.0, 15.0);
/// Lenticular: `plates`, how many are stacked (whole plates).
pub const CLOUD_LENTICULAR_PLATES_RANGE: (f32, f32) = (1.0, 6.0);
/// Lenticular: `thickness_m`, a plate's thickness at its middle, metres.
pub const CLOUD_LENTICULAR_THICKNESS_RANGE: (f32, f32) = (50.0, 800.0);
/// Lenticular: `gap_m`, the air between two plates, metres.
pub const CLOUD_LENTICULAR_GAP_RANGE: (f32, f32) = (50.0, 800.0);
/// Lenticular: `smoothness`.
pub const CLOUD_LENTICULAR_SMOOTHNESS_RANGE: (f32, f32) = (0.0, 1.0);
/// Lenticular: `striations`.
pub const CLOUD_LENTICULAR_STRIATIONS_RANGE: (f32, f32) = (0.0, 1.0);

/// The cloud layers (`SkyParams::cloud_stack`).
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct CloudStackParams {
    /// CLOUD_COVER_RANGE (1): the Clouds entry's Cover over every layer; 0 draws no cloud at all.
    pub cover: f32,
    /// The wind the layers blow from, compass degrees, wrapped into 0..360. None: `seeded_wind_deg(map seed)`.
    pub wind_deg: Option<f32>,
    /// CLOUD_MARCH_STEPS_RANGE (64): every marched layer's step budget.
    pub march_steps: u32,
    /// Any number of layers, never truncated. Their order means nothing to the drawing: height does.
    pub layers: Vec<CloudLayerParams>,
}

impl Default for CloudStackParams {
    fn default() -> Self {
        CloudStackParams { cover: 1.0, wind_deg: None, march_steps: 64, layers: Vec::new() }
    }
}

/// One cloud layer: a type (`genus`) with its block, and the rows every type has (design §4.1).
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct CloudLayerParams {
    /// The row's eye.
    pub enabled: bool,
    /// Up to 64 characters; "" lists as `cloud_display_name`'s "<Type> N".
    pub name: String,
    /// One of CLOUD_GENUS_NAMES, read through `genus()`. A name this build does not know is kept and not drawn.
    pub genus: String,
    /// The named form the layer was set to (`set_species`): a WMO species of its type, as `cloud_species` lists it
    /// ("congestus"), kept only while the rows hold it (an edit that leaves the form drops it, `settle_species`). A name
    /// its type does not have in this build is kept as written and never reported. None: none named (a new layer, a
    /// lifted one, an older file). The Species row shows what the rows hold (`species_of`).
    pub species: Option<String>,
    /// 0..=CLOUD_SEED_MAX. 0: the map's own streams (a lifted layer's); `cloud_layer_seed` folds the others.
    pub seed: u32,
    /// CLOUD_COVER_RANGE: the share of sky the layer hides, times the Clouds Cover. None: the type's default.
    pub cover: Option<f32>,
    /// The layer's own wind (Own wind), wrapped into 0..360. None: the Clouds wind.
    pub wind_deg: Option<f32>,
    /// CLOUD_DRIFT_RANGE, km downwind. None: 0.
    pub drift_km: Option<f32>,
    /// CLOUD_BRIGHTNESS_RANGE. None: 1.
    pub brightness: Option<f32>,
    /// CLOUD_SKY_LIGHT_RANGE. None: 1.
    pub sky_light: Option<f32>,
    /// CLOUD_LINING_RANGE. None: `CloudGenus::default_lining`.
    pub lining: Option<f32>,
    /// CLOUD_SHADE_BELOW_RANGE. None: `CloudGenus::default_shade_below`.
    pub shade_below: Option<f32>,
    /// The type blocks, in chart order; phases 2-4 insert theirs, in chart order, before `classic`. None reads as the
    /// type's defaults; a block the genus does not read is kept.
    pub cirrus: Option<CirrusParams>,
    pub cirrostratus: Option<CirrostratusParams>,
    pub altocumulus: Option<AltocumulusParams>,
    pub nimbostratus: Option<NimbostratusParams>,
    pub stratus: Option<StratusParams>,
    pub stratocumulus: Option<StratocumulusParams>,
    pub cumulus: Option<CumulusParams>,
    pub cumulonimbus: Option<CumulonimbusParams>,
    pub lenticular: Option<LenticularParams>,
    /// Set by the lift only and never shown (design §1.2): the layer is drawn as the older code drew it until a row
    /// that code cannot draw changes (`CloudLayerParams::edited`). Always the last field.
    pub classic: Option<ClassicLook>,
}

impl Default for CloudLayerParams {
    /// A shown Cumulus with no name, seed 0 and every Option None: the template a patched layer starts from.
    fn default() -> Self {
        CloudLayerParams {
            enabled: true,
            name: String::new(),
            genus: CloudGenus::Cumulus.as_str().to_string(),
            species: None,
            seed: 0,
            cover: None,
            wind_deg: None,
            drift_km: None,
            brightness: None,
            sky_light: None,
            lining: None,
            shade_below: None,
            cirrus: None,
            cirrostratus: None,
            altocumulus: None,
            nimbostratus: None,
            stratus: None,
            stratocumulus: None,
            cumulus: None,
            cumulonimbus: None,
            lenticular: None,
            classic: None,
        }
    }
}

/// Cirrus (a sheet, the ice family): wind-combed streaks with hooked ends.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct CirrusParams {
    /// CLOUD_CIRRUS_BASE_RANGE (9000), metres.
    pub base_m: f32,
    /// CLOUD_CIRRUS_SIZE_RANGE (12), km.
    pub size_km: f32,
    /// CLOUD_CIRRUS_STREAK_LENGTH_RANGE (7): how much longer the streaks run along the wind than across it.
    pub streak_length: f32,
    /// 0..=1 (0.6): toward ridged fibres.
    pub fineness: f32,
    /// 0..=1 (0.4): the ends curl.
    pub hooks: f32,
    /// 0..=1 (0.2): sheared and smeared downwind.
    pub fall_streaks: f32,
    /// 0..=1 (0.3): a broad mask.
    pub patchiness: f32,
    /// 0..=1 (0.5).
    pub opacity: f32,
}

impl Default for CirrusParams {
    fn default() -> Self {
        CirrusParams { base_m: 9000.0, size_km: 12.0, streak_length: 7.0, fineness: 0.6, hooks: 0.4, fall_streaks: 0.2, patchiness: 0.3, opacity: 0.5 }
    }
}

/// Cirrostratus (a sheet): a thin milky veil of ice high up, the sun's glow and its 22-degree halo through it, the blue
/// of the sky showing.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct CirrostratusParams {
    /// CLOUD_CIRROSTRATUS_BASE_RANGE (8500), metres.
    pub base_m: f32,
    /// CLOUD_CIRROSTRATUS_SIZE_RANGE (2), km: how large its mottles are (its fibres are finer, its veil's patches
    /// larger).
    pub size_km: f32,
    /// 0..=1 (0.45): how thick the veil is, from a faint whitening of the blue to a white sheet that dims the sun.
    pub milkiness: f32,
    /// 0..=1 (0.4): thicker and thinner patches in it.
    pub mottling: f32,
    /// 0..=1 (0.2): fibres along the wind (fibratus).
    pub fibres: f32,
    /// 0..=1 (0): the 22-degree ring round the sun its ice crystals bend the light into (none by default: halos are
    /// occasional, and the real veils 096_1228 and 107_1127 show none).
    pub halo: f32,
}

impl Default for CirrostratusParams {
    fn default() -> Self {
        CirrostratusParams { base_m: 8500.0, size_km: 2.0, milkiness: 0.45, mottling: 0.4, fibres: 0.2, halo: 0.0 }
    }
}

/// Altocumulus (a marched volume on the GPU): a mid-level deck of small rounded cloudlets in rows, grey sides and
/// bright rims, blue between (the mackerel sky).
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct AltocumulusParams {
    /// CLOUD_ALTOCUMULUS_BASE_RANGE (4000), metres.
    pub base_m: f32,
    /// CLOUD_ALTOCUMULUS_CELL_RANGE (400), metres: how far apart the cloudlets stand (a cloudlet some 4 degrees wide overhead
    /// at the default Base, the chart's 1 to 5).
    pub cell_m: f32,
    /// 0..=1 (0.3): from cloudlets at random to a lattice (the real decks' rows are a tendency, not a print).
    pub regularity: f32,
    /// 0..=1 (0.25): the cloudlets line up in rows across the wind, blue between the rows (the mackerel sky's).
    pub rows: f32,
    /// 0..=1 (0): some cloudlets rise into turrets (castellanus).
    pub turrets: f32,
    /// 0..=1 (0.7): how far a cloudlet's edge fades, from a crisp outline (lenticularis) to soft tufts (floccus).
    pub softness: f32,
}

impl Default for AltocumulusParams {
    fn default() -> Self {
        AltocumulusParams { base_m: 4000.0, cell_m: 400.0, regularity: 0.3, rows: 0.25, turrets: 0.0, softness: 0.7 }
    }
}

/// Nimbostratus (a slab; Volume option): a closed dark grey deck with rain curtains.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct NimbostratusParams {
    /// CLOUD_NIMBOSTRATUS_BASE_RANGE (1000), metres.
    pub base_m: f32,
    /// CLOUD_NIMBOSTRATUS_TOPS_RANGE (5000), metres: at least CLOUD_MIN_THICKNESS_M over the base, and a NaN lands
    /// 4000 m over it. A marched layer's.
    pub tops_m: f32,
    /// CLOUD_NIMBOSTRATUS_SIZE_RANGE (8), km.
    pub size_km: f32,
    /// 0..=1 (0.6): the slab's optical depth, 60 + 300 Darkness^2.
    pub darkness: f32,
    /// 0..=1 (0.4).
    pub unevenness: f32,
    /// 0..=1 (0.3): tears the lower edge.
    pub ragged_base: f32,
    /// 0..=1 (0.3): a dark ragged sheet under it.
    pub scud: f32,
    /// CLOUD_NIMBOSTRATUS_SCUD_HEIGHT_RANGE (500), metres.
    pub scud_height_m: f32,
    /// 0..=1 (0.5): rain hanging in its own alpha.
    pub rain_curtains: f32,
    /// Real depth on release (false).
    pub volume: bool,
    /// CLOUD_DENSITY_RANGE (1): the march's.
    pub density: f32,
}

impl Default for NimbostratusParams {
    fn default() -> Self {
        NimbostratusParams {
            base_m: 1000.0,
            tops_m: 5000.0,
            size_km: 8.0,
            darkness: 0.6,
            unevenness: 0.4,
            ragged_base: 0.3,
            scud: 0.3,
            scud_height_m: 500.0,
            rain_curtains: 0.5,
            volume: false,
            density: 1.0,
        }
    }
}

/// Stratus (a slab): a low grey layer with a soft mottle, ragged patches with blue gaps below Cover 1.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct StratusParams {
    /// CLOUD_STRATUS_BASE_RANGE (400), metres.
    pub base_m: f32,
    /// CLOUD_STRATUS_SIZE_RANGE (6), km.
    pub size_km: f32,
    /// CLOUD_STRATUS_DENSITY_RANGE (0.35): the slab's optical depth 2 + 65 Density, not a Volume's Density.
    pub density: f32,
    /// 0..=1 (0.25).
    pub mottling: f32,
    /// 0..=1 (0.5): the mask stretched along the wind.
    pub streaks: f32,
    /// 0..=1 (0.4).
    pub ragged_edges: f32,
}

impl Default for StratusParams {
    fn default() -> Self {
        StratusParams { base_m: 400.0, size_km: 6.0, density: 0.35, mottling: 0.25, streaks: 0.5, ragged_edges: 0.4 }
    }
}

/// Stratocumulus (a sheet, the heap family; Volume option): lumpy clumps with grey bases. Older decks lift to it.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct StratocumulusParams {
    /// CLOUD_STRATOCUMULUS_BASE_RANGE (1200), metres.
    pub base_m: f32,
    /// CLOUD_STRATOCUMULUS_TOPS_RANGE (1700), metres: over the base, and a NaN lands 500 m over it.
    pub tops_m: f32,
    /// CLOUD_STRATOCUMULUS_SIZE_RANGE (2.5), km.
    pub size_km: f32,
    /// 0..=1 (0.5): the edge band; an older deck's 1 - sharpness.
    pub softness: f32,
    /// 0..=1 (0.5, the older deck's self-shadow).
    pub shading: f32,
    /// 0..=1 (0.5).
    pub billows: f32,
    /// 0..=1 (0.3).
    pub rolls: f32,
    /// Real depth on release (false).
    pub volume: bool,
    /// CLOUD_DENSITY_RANGE (1).
    pub density: f32,
}

impl Default for StratocumulusParams {
    fn default() -> Self {
        StratocumulusParams { base_m: 1200.0, tops_m: 1700.0, size_km: 2.5, softness: 0.5, shading: 0.5, billows: 0.5, rolls: 0.3, volume: false, density: 1.0 }
    }
}

/// Cumulus (a shell from phase 3, a sheet in phase 1): bright heaps with flat grey bases.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct CumulusParams {
    /// CLOUD_CUMULUS_BASE_RANGE (1200), metres.
    pub base_m: f32,
    /// CLOUD_CUMULUS_TOPS_RANGE (2800), metres: over the base, and a NaN lands 1600 m over it.
    pub tops_m: f32,
    /// CLOUD_CUMULUS_SIZE_RANGE (1.5), km.
    pub size_km: f32,
    /// 0..=1 (0.7): an older volume's erosion.
    pub puffiness: f32,
    /// 0..=1 (0.7).
    pub flat_base: f32,
    /// 0..=1 (0.3).
    pub towers: f32,
    /// 0..=1 (0.3).
    pub clustering: f32,
    /// 0..=1 (0).
    pub streets: f32,
    /// 0..=1 (0.2).
    pub lean: f32,
    /// 0..=1 (0.4).
    pub softness: f32,
    /// Real depth on release (true: phase 3 marches it, so files keep their meaning).
    pub volume: bool,
    /// CLOUD_DENSITY_RANGE (1).
    pub density: f32,
}

impl Default for CumulusParams {
    fn default() -> Self {
        CumulusParams {
            base_m: 1200.0,
            tops_m: 2800.0,
            size_km: 1.5,
            puffiness: 0.7,
            flat_base: 0.7,
            towers: 0.3,
            clustering: 0.3,
            streets: 0.0,
            lean: 0.2,
            softness: 0.4,
            volume: true,
            density: 1.0,
        }
    }
}

/// Cumulonimbus (a placed volume, design §4.3): one storm cloud at (azimuth, distance), a sunlit tower of turrets, an
/// anvil spreading downwind at its top, a dark base with a rain shaft under it.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct CumulonimbusParams {
    /// CLOUD_CUMULONIMBUS_BASE_RANGE (1000), metres.
    pub base_m: f32,
    /// CLOUD_CUMULONIMBUS_TOPS_RANGE (12000), metres: the anvil's top.
    pub tops_m: f32,
    /// Where it stands, compass degrees, wrapped into 0..360 (0; "+ Cloud" and Storm put it opposite the sun).
    pub azimuth_deg: f32,
    /// CLOUD_CUMULONIMBUS_DISTANCE_RANGE (25), km to the tower's middle.
    pub distance_km: f32,
    /// CLOUD_CUMULONIMBUS_TOWER_RANGE (8), km: the tower's width.
    pub tower_km: f32,
    /// 0..=1 (0.6): how far the anvil spreads downwind.
    pub anvil: f32,
    /// CLOUD_CUMULONIMBUS_ANVIL_THICKNESS_RANGE (2500), metres: at the tower.
    pub anvil_m: f32,
    /// 0..=1 (0.3): the dome the updraft pushes over the anvil.
    pub overshoot: f32,
    /// CLOUD_CUMULONIMBUS_LEAN_RANGE (0), degrees downwind.
    pub lean_deg: f32,
    /// 0..=1 (0.7): how deep the tower's billows cut.
    pub puffiness: f32,
    /// 0..=1 (0.5): how dark the base and the shaded tower are.
    pub darkness: f32,
    /// 0..=1 (0.6): the rain falling from the base.
    pub rain_shaft: f32,
    /// 0..=1 (0): pouches under the anvil.
    pub mammatus: f32,
    /// CLOUD_DENSITY_RANGE (1).
    pub density: f32,
}

impl Default for CumulonimbusParams {
    fn default() -> Self {
        CumulonimbusParams {
            base_m: 1000.0,
            tops_m: 12_000.0,
            azimuth_deg: 0.0,
            distance_km: 25.0,
            tower_km: 8.0,
            anvil: 0.6,
            anvil_m: 2500.0,
            overshoot: 0.3,
            lean_deg: 0.0,
            puffiness: 0.7,
            darkness: 0.5,
            rain_shaft: 0.6,
            mammatus: 0.0,
            density: 1.0,
        }
    }
}

/// Lenticular (a placed lens, design §4.3): smooth lens plates stacked over each other at (azimuth, distance), their
/// long axis across the wind.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct LenticularParams {
    /// CLOUD_LENTICULAR_BASE_RANGE (5000), metres: the lowest plate's underside.
    pub base_m: f32,
    /// Where it stands, compass degrees, wrapped into 0..360 (0; "+ Cloud" puts it a quarter turn off the sun).
    pub azimuth_deg: f32,
    /// CLOUD_LENTICULAR_DISTANCE_RANGE (15), km to the stack's middle.
    pub distance_km: f32,
    /// CLOUD_LENTICULAR_LENGTH_RANGE (14), km across the wind.
    pub length_km: f32,
    /// CLOUD_LENTICULAR_WIDTH_RANGE (6), km along the wind.
    pub width_km: f32,
    /// CLOUD_LENTICULAR_PLATES_RANGE (3): drawn as whole plates (rounded).
    pub plates: f32,
    /// CLOUD_LENTICULAR_THICKNESS_RANGE (300), metres: a plate at its middle.
    pub thickness_m: f32,
    /// CLOUD_LENTICULAR_GAP_RANGE (250), metres between plates.
    pub gap_m: f32,
    /// 0..=1 (0.8): a softer rim and a smoother surface.
    pub smoothness: f32,
    /// 0..=1 (0.4): the faint concentric bands along the plates.
    pub striations: f32,
    /// CLOUD_DENSITY_RANGE (1).
    pub density: f32,
}

impl Default for LenticularParams {
    fn default() -> Self {
        LenticularParams {
            base_m: 5000.0,
            azimuth_deg: 0.0,
            distance_km: 15.0,
            length_km: 14.0,
            width_km: 6.0,
            plates: 3.0,
            thickness_m: 300.0,
            gap_m: 250.0,
            smoothness: 0.8,
            striations: 0.4,
            density: 1.0,
        }
    }
}

/// What the lift keeps of an older layer (design §1.2): its amounts after the kind's floor and the height whose sun
/// lit it. A classic layer is drawn by today's code from its parts until a row that code cannot draw changes.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct ClassicLook {
    /// 0..=1 (0): the older deck's coverage, or the cirrus amount, as drawn.
    pub coverage: f32,
    /// 0..=1 (0.5).
    pub sharpness: f32,
    /// CLOUD_CLASSIC_LIGHT_RANGE (2000), metres: the deck's altitude, the volume's middle.
    pub light_m: f32,
}

impl Default for ClassicLook {
    fn default() -> Self {
        ClassicLook { coverage: 0.0, sharpness: 0.5, light_m: 2000.0 }
    }
}

/// The rows a classic layer keeps through (design §1.2's list with the tops and the puffiness): today's code draws
/// them all from the layer's parts. Any other row, the Type included, drops `classic` (`CloudLayerParams::edited`).
pub const CLASSIC_KEYS: [&str; 14] = ["enabled", "name", "cover", "seed", "wind_deg", "drift_km", "brightness", "base_m", "tops_m", "size_km", "softness", "volume", "density", "puffiness"];

/// clamp()'s rule for an optional number: Some(NaN) is None (the field's own default), anything else is held into
/// `lo..=hi`, infinities to the bound; a change is reported.
fn opt_range(f: &mut Fixes, prefix: &str, field: &str, value: &mut Option<f32>, (lo, hi): (f32, f32)) {
    if let Some(old) = *value {
        if old.is_nan() {
            *value = None;
            f.push(prefix, field);
        } else if !(lo..=hi).contains(&old) {
            *value = Some(old.clamp(lo, hi));
            f.push(prefix, field);
        }
    }
}

/// A wind's compass heading: None (the Clouds wind, or the map seed's angle) when it is not a finite number, else
/// wrapped into [0, 360) by `Fixes::wrap360`.
fn opt_wrap360(f: &mut Fixes, prefix: &str, field: &str, value: &mut Option<f32>) {
    if let Some(old) = value.as_mut() {
        if old.is_finite() {
            f.wrap360(prefix, field, old, 0.0);
        } else {
            *value = None;
            f.push(prefix, field);
        }
    }
}

/// A block's `tops_m` after its `base_m` (`VolumetricParams::clamp_fields`' rule): at least CLOUD_MIN_THICKNESS_M
/// over the base, up to the range's top, or exactly that clearance under a base above it; a NaN lands the type's own
/// `thickness` over the base.
fn hold_tops(f: &mut Fixes, prefix: &str, tops: &mut f32, base: f32, (lo, hi): (f32, f32), thickness: f32) {
    let lowest = (base + CLOUD_MIN_THICKNESS_M).max(lo);
    let highest = hi.max(lowest);
    f.range(prefix, "tops_m", tops, lowest, highest, (base + thickness).clamp(lowest, highest));
}

impl CloudStackParams {
    pub(super) fn clamp_fields(&mut self, prefix: &str, f: &mut Fixes) {
        let d = CloudStackParams::default();
        f.range(prefix, "cover", &mut self.cover, CLOUD_COVER_RANGE.0, CLOUD_COVER_RANGE.1, d.cover);
        opt_wrap360(f, prefix, "wind_deg", &mut self.wind_deg);
        f.int(prefix, "march_steps", &mut self.march_steps, CLOUD_MARCH_STEPS_RANGE.0, CLOUD_MARCH_STEPS_RANGE.1);
        // Any number of layers: the list is never truncated.
        for (i, layer) in self.layers.iter_mut().enumerate() {
            layer.clamp_fields(&join_path(prefix, &format!("layers[{i}]")), f);
        }
    }
}

impl CloudLayerParams {
    pub(super) fn clamp_fields(&mut self, prefix: &str, f: &mut Fixes) {
        if self.name.chars().count() > LIGHT_NAME_LIMIT {
            self.name = self.name.chars().take(LIGHT_NAME_LIMIT).collect();
            f.push(prefix, "name");
        }
        // A known genus spelled loosely is normalised; one this build does not know is kept as written and never
        // reported (no Fixes::choice fallback), so a file from a newer build keeps it.
        let normal = self.genus.trim().to_ascii_lowercase();
        if CLOUD_GENUS_NAMES.contains(&normal.as_str()) && normal != self.genus {
            self.genus = normal;
            f.push(prefix, "genus");
        }
        // A species of the layer's type spelled loosely ("  Congestus", "Cumulus congestus") is written as the type's
        // table has it and reported; one the type does not have in this build is kept as written and never reported.
        let species = match (self.genus(), self.species.as_deref()) {
            (Some(genus), Some(name)) => crate::cloud_species::find_species(genus, name).map(|species| species.name).filter(|table| *table != name),
            _ => None,
        };
        if let Some(name) = species {
            self.species = Some(name.to_string());
            f.push(prefix, "species");
        }
        f.int(prefix, "seed", &mut self.seed, 0, CLOUD_SEED_MAX);
        opt_range(f, prefix, "cover", &mut self.cover, CLOUD_COVER_RANGE);
        opt_wrap360(f, prefix, "wind_deg", &mut self.wind_deg);
        opt_range(f, prefix, "drift_km", &mut self.drift_km, CLOUD_DRIFT_RANGE);
        opt_range(f, prefix, "brightness", &mut self.brightness, CLOUD_BRIGHTNESS_RANGE);
        opt_range(f, prefix, "sky_light", &mut self.sky_light, CLOUD_SKY_LIGHT_RANGE);
        opt_range(f, prefix, "lining", &mut self.lining, CLOUD_LINING_RANGE);
        opt_range(f, prefix, "shade_below", &mut self.shade_below, CLOUD_SHADE_BELOW_RANGE);
        // Every block present, whatever the genus: a layer keeps the numbers of the types it was.
        if let Some(block) = self.cirrus.as_mut() {
            block.clamp_fields(&join_path(prefix, "cirrus"), f);
        }
        if let Some(block) = self.cirrostratus.as_mut() {
            block.clamp_fields(&join_path(prefix, "cirrostratus"), f);
        }
        if let Some(block) = self.altocumulus.as_mut() {
            block.clamp_fields(&join_path(prefix, "altocumulus"), f);
        }
        if let Some(block) = self.nimbostratus.as_mut() {
            block.clamp_fields(&join_path(prefix, "nimbostratus"), f);
        }
        if let Some(block) = self.stratus.as_mut() {
            block.clamp_fields(&join_path(prefix, "stratus"), f);
        }
        if let Some(block) = self.stratocumulus.as_mut() {
            block.clamp_fields(&join_path(prefix, "stratocumulus"), f);
        }
        if let Some(block) = self.cumulus.as_mut() {
            block.clamp_fields(&join_path(prefix, "cumulus"), f);
        }
        if let Some(block) = self.cumulonimbus.as_mut() {
            block.clamp_fields(&join_path(prefix, "cumulonimbus"), f);
        }
        if let Some(block) = self.lenticular.as_mut() {
            block.clamp_fields(&join_path(prefix, "lenticular"), f);
        }
        if let Some(block) = self.classic.as_mut() {
            block.clamp_fields(&join_path(prefix, "classic"), f);
        }
    }

    /// What "+ Cloud", set_clouds' add and a weather pick make (design §5.3): the type's block at its defaults, the
    /// type's new-layer Cover (`new_cover`: None, the type's default, but a Stratus' 1), the lowest free "<Type> N"
    /// among `layers` and a fresh seed none of them holds (`free_cloud_seed`).
    pub fn new(genus: CloudGenus, layers: &[CloudLayerParams], map_seed: u32) -> CloudLayerParams {
        let mut layer = CloudLayerParams {
            name: next_cloud_name(layers, genus),
            genus: genus.as_str().to_string(),
            seed: free_cloud_seed(layers, map_seed, genus),
            cover: genus.new_cover(),
            ..CloudLayerParams::default()
        };
        layer.make_block(genus);
        layer
    }

    /// The type `genus` names; None for a name this build does not know.
    pub fn genus(&self) -> Option<CloudGenus> {
        CloudGenus::of(&self.genus)
    }

    /// The block's `base_m` through the type's defaults; 0 for a genus with no block in this build. Rows sort by it,
    /// and the rain rule reads it.
    pub fn base_m(&self) -> f32 {
        match self.genus() {
            Some(CloudGenus::Cirrus) => self.cirrus_params().base_m,
            Some(CloudGenus::Cirrostratus) => self.cirrostratus_params().base_m,
            Some(CloudGenus::Altocumulus) => self.altocumulus_params().base_m,
            Some(CloudGenus::Nimbostratus) => self.nimbostratus_params().base_m,
            Some(CloudGenus::Stratus) => self.stratus_params().base_m,
            Some(CloudGenus::Stratocumulus) => self.stratocumulus_params().base_m,
            Some(CloudGenus::Cumulus) => self.cumulus_params().base_m,
            Some(CloudGenus::Cumulonimbus) => self.cumulonimbus_params().base_m,
            Some(CloudGenus::Lenticular) => self.lenticular_params().base_m,
            _ => 0.0,
        }
    }

    /// The Cirrus block as clamp() holds it, or its defaults when there is none (`lightning_params`' rule).
    pub fn cirrus_params(&self) -> CirrusParams {
        self.cirrus.as_ref().map(CirrusParams::clamped).unwrap_or_default()
    }

    /// The Cirrostratus block as clamp() holds it, or its defaults.
    pub fn cirrostratus_params(&self) -> CirrostratusParams {
        self.cirrostratus.as_ref().map(CirrostratusParams::clamped).unwrap_or_default()
    }

    /// The Altocumulus block as clamp() holds it, or its defaults.
    pub fn altocumulus_params(&self) -> AltocumulusParams {
        self.altocumulus.as_ref().map(AltocumulusParams::clamped).unwrap_or_default()
    }

    /// The Nimbostratus block as clamp() holds it, or its defaults.
    pub fn nimbostratus_params(&self) -> NimbostratusParams {
        self.nimbostratus.as_ref().map(NimbostratusParams::clamped).unwrap_or_default()
    }

    /// The Stratus block as clamp() holds it, or its defaults.
    pub fn stratus_params(&self) -> StratusParams {
        self.stratus.as_ref().map(StratusParams::clamped).unwrap_or_default()
    }

    /// The Stratocumulus block as clamp() holds it, or its defaults.
    pub fn stratocumulus_params(&self) -> StratocumulusParams {
        self.stratocumulus.as_ref().map(StratocumulusParams::clamped).unwrap_or_default()
    }

    /// The Cumulus block as clamp() holds it, or its defaults.
    pub fn cumulus_params(&self) -> CumulusParams {
        self.cumulus.as_ref().map(CumulusParams::clamped).unwrap_or_default()
    }

    /// The Cumulonimbus block as clamp() holds it, or its defaults.
    pub fn cumulonimbus_params(&self) -> CumulonimbusParams {
        self.cumulonimbus.as_ref().map(CumulonimbusParams::clamped).unwrap_or_default()
    }

    /// The Lenticular block as clamp() holds it, or its defaults.
    pub fn lenticular_params(&self) -> LenticularParams {
        self.lenticular.as_ref().map(LenticularParams::clamped).unwrap_or_default()
    }

    /// A Type change (design §5.4): every block is kept and the new type's is made if missing, `classic` is dropped,
    /// the species name picked goes (it named a form of the old type; several types share a species' name), a Cover
    /// that is None, the old type's default or the old type's new-layer Cover becomes the new type's new-layer Cover
    /// (`new_cover`: None, its default, for most types), and a name that is "" or the old type's "<Label> N" becomes
    /// `next_cloud_name(layers, genus)`. Picking the type the layer has changes nothing.
    pub fn set_genus(&mut self, genus: CloudGenus, layers: &[CloudLayerParams]) {
        let old = self.genus();
        if old == Some(genus) {
            return;
        }
        let name = self.name.trim();
        let default_name = name.is_empty()
            || old.is_some_and(|old| {
                name.strip_prefix(old.label()).and_then(|rest| rest.strip_prefix(' ')).and_then(|n| n.parse::<u32>().ok()).is_some_and(|n| n >= 1)
            });
        // A new Stratus' Cover 1 is the type's, not the person's: as a Cover at the old type's default, it gives way to
        // the new type's (a Stratus made and turned into a Cumulus is not a closed sky of heaps).
        if self.cover.is_none() || old.is_some_and(|old| self.cover == Some(old.default_cover()) || (self.cover.is_some() && self.cover == old.new_cover())) {
            self.cover = genus.new_cover();
        }
        self.genus = genus.as_str().to_string();
        self.make_block(genus);
        self.classic = None;
        self.species = None;
        if default_name {
            self.name = next_cloud_name(layers, genus);
        }
    }

    /// The block `genus` reads, made at its defaults when the layer has none (a type not built yet has none).
    fn make_block(&mut self, genus: CloudGenus) {
        match genus {
            CloudGenus::Cirrus if self.cirrus.is_none() => self.cirrus = Some(CirrusParams::default()),
            CloudGenus::Cirrostratus if self.cirrostratus.is_none() => self.cirrostratus = Some(CirrostratusParams::default()),
            CloudGenus::Altocumulus if self.altocumulus.is_none() => self.altocumulus = Some(AltocumulusParams::default()),
            CloudGenus::Nimbostratus if self.nimbostratus.is_none() => self.nimbostratus = Some(NimbostratusParams::default()),
            CloudGenus::Stratus if self.stratus.is_none() => self.stratus = Some(StratusParams::default()),
            CloudGenus::Stratocumulus if self.stratocumulus.is_none() => self.stratocumulus = Some(StratocumulusParams::default()),
            CloudGenus::Cumulus if self.cumulus.is_none() => self.cumulus = Some(CumulusParams::default()),
            CloudGenus::Cumulonimbus if self.cumulonimbus.is_none() => self.cumulonimbus = Some(CumulonimbusParams::default()),
            CloudGenus::Lenticular if self.lenticular.is_none() => self.lenticular = Some(LenticularParams::default()),
            _ => {}
        }
    }

    /// The number a row of the layer shows, by the row's name (the names `edited` and set_clouds use): Cover as the row
    /// shows it (`shown_cover`), every type's optional numbers at their defaults, its type's own numbers as clamp()
    /// holds its block (`block_rows!`). None for a name its type has no number for, or a type with no block in this
    /// build.
    pub fn row(&self, key: &str) -> Option<f32> {
        let genus = self.genus()?;
        match key {
            "cover" => Some(self.shown_cover()),
            "drift_km" => Some(self.drift_km.unwrap_or(0.0)),
            "brightness" => Some(self.brightness.unwrap_or(1.0)),
            "sky_light" => Some(self.sky_light.unwrap_or(1.0)),
            "lining" => Some(self.lining.unwrap_or(genus.default_lining())),
            "shade_below" => Some(self.shade_below.unwrap_or(genus.default_shade_below())),
            _ => match genus {
                CloudGenus::Cirrus => self.cirrus_params().row(key),
                CloudGenus::Cirrostratus => self.cirrostratus_params().row(key),
                CloudGenus::Altocumulus => self.altocumulus_params().row(key),
                CloudGenus::Nimbostratus => self.nimbostratus_params().row(key),
                CloudGenus::Stratus => self.stratus_params().row(key),
                CloudGenus::Stratocumulus => self.stratocumulus_params().row(key),
                CloudGenus::Cumulus => self.cumulus_params().row(key),
                CloudGenus::Cumulonimbus => self.cumulonimbus_params().row(key),
                CloudGenus::Lenticular => self.lenticular_params().row(key),
                _ => None,
            },
        }
    }

    /// Writes the row `key` as the inspector's row writes it: an optional number of every type is set, a number of
    /// its type's block is set in the block, made at its defaults when missing. False, with the layer untouched, for a
    /// name `row` does not know. The caller runs the classic rule (`edited`) after it, as after any row.
    pub fn set_row(&mut self, key: &str, value: f32) -> bool {
        let Some(genus) = self.genus() else {
            return false;
        };
        let common = match key {
            "cover" => Some(&mut self.cover),
            "drift_km" => Some(&mut self.drift_km),
            "brightness" => Some(&mut self.brightness),
            "sky_light" => Some(&mut self.sky_light),
            "lining" => Some(&mut self.lining),
            "shade_below" => Some(&mut self.shade_below),
            _ => None,
        };
        if let Some(field) = common {
            *field = Some(value);
            return true;
        }
        if self.row(key).is_none() {
            return false;
        }
        self.make_block(genus);
        let field = match genus {
            CloudGenus::Cirrus => self.cirrus.as_mut().and_then(|block| block.row_mut(key)),
            CloudGenus::Cirrostratus => self.cirrostratus.as_mut().and_then(|block| block.row_mut(key)),
            CloudGenus::Altocumulus => self.altocumulus.as_mut().and_then(|block| block.row_mut(key)),
            CloudGenus::Nimbostratus => self.nimbostratus.as_mut().and_then(|block| block.row_mut(key)),
            CloudGenus::Stratus => self.stratus.as_mut().and_then(|block| block.row_mut(key)),
            CloudGenus::Stratocumulus => self.stratocumulus.as_mut().and_then(|block| block.row_mut(key)),
            CloudGenus::Cumulus => self.cumulus.as_mut().and_then(|block| block.row_mut(key)),
            CloudGenus::Cumulonimbus => self.cumulonimbus.as_mut().and_then(|block| block.row_mut(key)),
            CloudGenus::Lenticular => self.lenticular.as_mut().and_then(|block| block.row_mut(key)),
            _ => None,
        };
        match field {
            Some(field) => {
                *field = value;
                true
            }
            None => false,
        }
    }
}

/// A block's numbers by their rows' names, so a row can be read and written by name (`CloudLayerParams::row`,
/// `set_row`): the species table names them so. `volume` is a tick, not a number, and has no row here. A type's block
/// lists every number it has.
macro_rules! block_rows {
    ($($block:ty { $($field:ident),* $(,)? })*) => {
        $(impl $block {
            /// The number of the row `key`; None for a row the block does not have.
            pub fn row(&self, key: &str) -> Option<f32> {
                match key {
                    $(stringify!($field) => Some(self.$field),)*
                    _ => None,
                }
            }

            /// The number of the row `key`, to write; None for a row the block does not have.
            pub fn row_mut(&mut self, key: &str) -> Option<&mut f32> {
                match key {
                    $(stringify!($field) => Some(&mut self.$field),)*
                    _ => None,
                }
            }
        })*
    };
}

block_rows! {
    CirrusParams { base_m, size_km, streak_length, fineness, hooks, fall_streaks, patchiness, opacity }
    CirrostratusParams { base_m, size_km, milkiness, mottling, fibres, halo }
    AltocumulusParams { base_m, cell_m, regularity, rows, turrets, softness }
    NimbostratusParams { base_m, tops_m, size_km, darkness, unevenness, ragged_base, scud, scud_height_m, rain_curtains, density }
    StratusParams { base_m, size_km, density, mottling, streaks, ragged_edges }
    StratocumulusParams { base_m, tops_m, size_km, softness, shading, billows, rolls, density }
    CumulusParams { base_m, tops_m, size_km, puffiness, flat_base, towers, clustering, streets, lean, softness, density }
    CumulonimbusParams { base_m, tops_m, azimuth_deg, distance_km, tower_km, anvil, anvil_m, overshoot, lean_deg, puffiness, darkness, rain_shaft, mammatus, density }
    LenticularParams { base_m, azimuth_deg, distance_km, length_km, width_km, plates, thickness_m, gap_m, smoothness, striations, density }
}

impl CirrusParams {
    pub(super) fn clamp_fields(&mut self, prefix: &str, f: &mut Fixes) {
        let d = CirrusParams::default();
        f.range(prefix, "base_m", &mut self.base_m, CLOUD_CIRRUS_BASE_RANGE.0, CLOUD_CIRRUS_BASE_RANGE.1, d.base_m);
        f.range(prefix, "size_km", &mut self.size_km, CLOUD_CIRRUS_SIZE_RANGE.0, CLOUD_CIRRUS_SIZE_RANGE.1, d.size_km);
        f.range(prefix, "streak_length", &mut self.streak_length, CLOUD_CIRRUS_STREAK_LENGTH_RANGE.0, CLOUD_CIRRUS_STREAK_LENGTH_RANGE.1, d.streak_length);
        f.range(prefix, "fineness", &mut self.fineness, CLOUD_CIRRUS_FINENESS_RANGE.0, CLOUD_CIRRUS_FINENESS_RANGE.1, d.fineness);
        f.range(prefix, "hooks", &mut self.hooks, CLOUD_CIRRUS_HOOKS_RANGE.0, CLOUD_CIRRUS_HOOKS_RANGE.1, d.hooks);
        f.range(prefix, "fall_streaks", &mut self.fall_streaks, CLOUD_CIRRUS_FALL_STREAKS_RANGE.0, CLOUD_CIRRUS_FALL_STREAKS_RANGE.1, d.fall_streaks);
        f.range(prefix, "patchiness", &mut self.patchiness, CLOUD_CIRRUS_PATCHINESS_RANGE.0, CLOUD_CIRRUS_PATCHINESS_RANGE.1, d.patchiness);
        f.range(prefix, "opacity", &mut self.opacity, CLOUD_CIRRUS_OPACITY_RANGE.0, CLOUD_CIRRUS_OPACITY_RANGE.1, d.opacity);
    }
}

impl CirrostratusParams {
    pub(super) fn clamp_fields(&mut self, prefix: &str, f: &mut Fixes) {
        let d = CirrostratusParams::default();
        f.range(prefix, "base_m", &mut self.base_m, CLOUD_CIRROSTRATUS_BASE_RANGE.0, CLOUD_CIRROSTRATUS_BASE_RANGE.1, d.base_m);
        f.range(prefix, "size_km", &mut self.size_km, CLOUD_CIRROSTRATUS_SIZE_RANGE.0, CLOUD_CIRROSTRATUS_SIZE_RANGE.1, d.size_km);
        f.range(prefix, "milkiness", &mut self.milkiness, CLOUD_CIRROSTRATUS_MILKINESS_RANGE.0, CLOUD_CIRROSTRATUS_MILKINESS_RANGE.1, d.milkiness);
        f.range(prefix, "mottling", &mut self.mottling, CLOUD_CIRROSTRATUS_MOTTLING_RANGE.0, CLOUD_CIRROSTRATUS_MOTTLING_RANGE.1, d.mottling);
        f.range(prefix, "fibres", &mut self.fibres, CLOUD_CIRROSTRATUS_FIBRES_RANGE.0, CLOUD_CIRROSTRATUS_FIBRES_RANGE.1, d.fibres);
        f.range(prefix, "halo", &mut self.halo, CLOUD_CIRROSTRATUS_HALO_RANGE.0, CLOUD_CIRROSTRATUS_HALO_RANGE.1, d.halo);
    }
}

impl AltocumulusParams {
    pub(super) fn clamp_fields(&mut self, prefix: &str, f: &mut Fixes) {
        let d = AltocumulusParams::default();
        f.range(prefix, "base_m", &mut self.base_m, CLOUD_ALTOCUMULUS_BASE_RANGE.0, CLOUD_ALTOCUMULUS_BASE_RANGE.1, d.base_m);
        f.range(prefix, "cell_m", &mut self.cell_m, CLOUD_ALTOCUMULUS_CELL_RANGE.0, CLOUD_ALTOCUMULUS_CELL_RANGE.1, d.cell_m);
        f.range(prefix, "regularity", &mut self.regularity, CLOUD_ALTOCUMULUS_REGULARITY_RANGE.0, CLOUD_ALTOCUMULUS_REGULARITY_RANGE.1, d.regularity);
        f.range(prefix, "rows", &mut self.rows, CLOUD_ALTOCUMULUS_ROWS_RANGE.0, CLOUD_ALTOCUMULUS_ROWS_RANGE.1, d.rows);
        f.range(prefix, "turrets", &mut self.turrets, CLOUD_ALTOCUMULUS_TURRETS_RANGE.0, CLOUD_ALTOCUMULUS_TURRETS_RANGE.1, d.turrets);
        f.range(prefix, "softness", &mut self.softness, CLOUD_ALTOCUMULUS_SOFTNESS_RANGE.0, CLOUD_ALTOCUMULUS_SOFTNESS_RANGE.1, d.softness);
    }
}

impl NimbostratusParams {
    pub(super) fn clamp_fields(&mut self, prefix: &str, f: &mut Fixes) {
        let d = NimbostratusParams::default();
        f.range(prefix, "base_m", &mut self.base_m, CLOUD_NIMBOSTRATUS_BASE_RANGE.0, CLOUD_NIMBOSTRATUS_BASE_RANGE.1, d.base_m);
        hold_tops(f, prefix, &mut self.tops_m, self.base_m, CLOUD_NIMBOSTRATUS_TOPS_RANGE, d.tops_m - d.base_m);
        f.range(prefix, "size_km", &mut self.size_km, CLOUD_NIMBOSTRATUS_SIZE_RANGE.0, CLOUD_NIMBOSTRATUS_SIZE_RANGE.1, d.size_km);
        f.range(prefix, "darkness", &mut self.darkness, CLOUD_NIMBOSTRATUS_DARKNESS_RANGE.0, CLOUD_NIMBOSTRATUS_DARKNESS_RANGE.1, d.darkness);
        f.range(prefix, "unevenness", &mut self.unevenness, CLOUD_NIMBOSTRATUS_UNEVENNESS_RANGE.0, CLOUD_NIMBOSTRATUS_UNEVENNESS_RANGE.1, d.unevenness);
        f.range(prefix, "ragged_base", &mut self.ragged_base, CLOUD_NIMBOSTRATUS_RAGGED_BASE_RANGE.0, CLOUD_NIMBOSTRATUS_RAGGED_BASE_RANGE.1, d.ragged_base);
        f.range(prefix, "scud", &mut self.scud, CLOUD_NIMBOSTRATUS_SCUD_RANGE.0, CLOUD_NIMBOSTRATUS_SCUD_RANGE.1, d.scud);
        // The Scud height follows the base as the drawing does: at most SCUD_GAP_M under it; with no room the range's
        // lowest, where the drawing skips the scud.
        let (lo, hi) = CLOUD_NIMBOSTRATUS_SCUD_HEIGHT_RANGE;
        let highest = (self.base_m - crate::cloud_slabs::SCUD_GAP_M).clamp(lo, hi);
        f.range(prefix, "scud_height_m", &mut self.scud_height_m, lo, highest, d.scud_height_m.min(highest));
        f.range(prefix, "rain_curtains", &mut self.rain_curtains, CLOUD_NIMBOSTRATUS_RAIN_CURTAINS_RANGE.0, CLOUD_NIMBOSTRATUS_RAIN_CURTAINS_RANGE.1, d.rain_curtains);
        f.range(prefix, "density", &mut self.density, CLOUD_DENSITY_RANGE.0, CLOUD_DENSITY_RANGE.1, d.density);
    }
}

impl StratusParams {
    pub(super) fn clamp_fields(&mut self, prefix: &str, f: &mut Fixes) {
        let d = StratusParams::default();
        f.range(prefix, "base_m", &mut self.base_m, CLOUD_STRATUS_BASE_RANGE.0, CLOUD_STRATUS_BASE_RANGE.1, d.base_m);
        f.range(prefix, "size_km", &mut self.size_km, CLOUD_STRATUS_SIZE_RANGE.0, CLOUD_STRATUS_SIZE_RANGE.1, d.size_km);
        f.range(prefix, "density", &mut self.density, CLOUD_STRATUS_DENSITY_RANGE.0, CLOUD_STRATUS_DENSITY_RANGE.1, d.density);
        f.range(prefix, "mottling", &mut self.mottling, CLOUD_STRATUS_MOTTLING_RANGE.0, CLOUD_STRATUS_MOTTLING_RANGE.1, d.mottling);
        f.range(prefix, "streaks", &mut self.streaks, CLOUD_STRATUS_STREAKS_RANGE.0, CLOUD_STRATUS_STREAKS_RANGE.1, d.streaks);
        f.range(prefix, "ragged_edges", &mut self.ragged_edges, CLOUD_STRATUS_RAGGED_EDGES_RANGE.0, CLOUD_STRATUS_RAGGED_EDGES_RANGE.1, d.ragged_edges);
    }
}

impl StratocumulusParams {
    pub(super) fn clamp_fields(&mut self, prefix: &str, f: &mut Fixes) {
        let d = StratocumulusParams::default();
        f.range(prefix, "base_m", &mut self.base_m, CLOUD_STRATOCUMULUS_BASE_RANGE.0, CLOUD_STRATOCUMULUS_BASE_RANGE.1, d.base_m);
        hold_tops(f, prefix, &mut self.tops_m, self.base_m, CLOUD_STRATOCUMULUS_TOPS_RANGE, d.tops_m - d.base_m);
        f.range(prefix, "size_km", &mut self.size_km, CLOUD_STRATOCUMULUS_SIZE_RANGE.0, CLOUD_STRATOCUMULUS_SIZE_RANGE.1, d.size_km);
        f.range(prefix, "softness", &mut self.softness, CLOUD_STRATOCUMULUS_SOFTNESS_RANGE.0, CLOUD_STRATOCUMULUS_SOFTNESS_RANGE.1, d.softness);
        f.range(prefix, "shading", &mut self.shading, CLOUD_STRATOCUMULUS_SHADING_RANGE.0, CLOUD_STRATOCUMULUS_SHADING_RANGE.1, d.shading);
        f.range(prefix, "billows", &mut self.billows, CLOUD_STRATOCUMULUS_BILLOWS_RANGE.0, CLOUD_STRATOCUMULUS_BILLOWS_RANGE.1, d.billows);
        f.range(prefix, "rolls", &mut self.rolls, CLOUD_STRATOCUMULUS_ROLLS_RANGE.0, CLOUD_STRATOCUMULUS_ROLLS_RANGE.1, d.rolls);
        f.range(prefix, "density", &mut self.density, CLOUD_DENSITY_RANGE.0, CLOUD_DENSITY_RANGE.1, d.density);
    }
}

impl CumulusParams {
    pub(super) fn clamp_fields(&mut self, prefix: &str, f: &mut Fixes) {
        let d = CumulusParams::default();
        f.range(prefix, "base_m", &mut self.base_m, CLOUD_CUMULUS_BASE_RANGE.0, CLOUD_CUMULUS_BASE_RANGE.1, d.base_m);
        hold_tops(f, prefix, &mut self.tops_m, self.base_m, CLOUD_CUMULUS_TOPS_RANGE, d.tops_m - d.base_m);
        f.range(prefix, "size_km", &mut self.size_km, CLOUD_CUMULUS_SIZE_RANGE.0, CLOUD_CUMULUS_SIZE_RANGE.1, d.size_km);
        f.range(prefix, "puffiness", &mut self.puffiness, CLOUD_CUMULUS_PUFFINESS_RANGE.0, CLOUD_CUMULUS_PUFFINESS_RANGE.1, d.puffiness);
        f.range(prefix, "flat_base", &mut self.flat_base, CLOUD_CUMULUS_FLAT_BASE_RANGE.0, CLOUD_CUMULUS_FLAT_BASE_RANGE.1, d.flat_base);
        f.range(prefix, "towers", &mut self.towers, CLOUD_CUMULUS_TOWERS_RANGE.0, CLOUD_CUMULUS_TOWERS_RANGE.1, d.towers);
        f.range(prefix, "clustering", &mut self.clustering, CLOUD_CUMULUS_CLUSTERING_RANGE.0, CLOUD_CUMULUS_CLUSTERING_RANGE.1, d.clustering);
        f.range(prefix, "streets", &mut self.streets, CLOUD_CUMULUS_STREETS_RANGE.0, CLOUD_CUMULUS_STREETS_RANGE.1, d.streets);
        f.range(prefix, "lean", &mut self.lean, CLOUD_CUMULUS_LEAN_RANGE.0, CLOUD_CUMULUS_LEAN_RANGE.1, d.lean);
        f.range(prefix, "softness", &mut self.softness, CLOUD_CUMULUS_SOFTNESS_RANGE.0, CLOUD_CUMULUS_SOFTNESS_RANGE.1, d.softness);
        f.range(prefix, "density", &mut self.density, CLOUD_DENSITY_RANGE.0, CLOUD_DENSITY_RANGE.1, d.density);
    }
}

impl CumulonimbusParams {
    pub(super) fn clamp_fields(&mut self, prefix: &str, f: &mut Fixes) {
        let d = CumulonimbusParams::default();
        f.range(prefix, "base_m", &mut self.base_m, CLOUD_CUMULONIMBUS_BASE_RANGE.0, CLOUD_CUMULONIMBUS_BASE_RANGE.1, d.base_m);
        hold_tops(f, prefix, &mut self.tops_m, self.base_m, CLOUD_CUMULONIMBUS_TOPS_RANGE, d.tops_m - d.base_m);
        f.wrap360(prefix, "azimuth_deg", &mut self.azimuth_deg, d.azimuth_deg);
        f.range(prefix, "distance_km", &mut self.distance_km, CLOUD_CUMULONIMBUS_DISTANCE_RANGE.0, CLOUD_CUMULONIMBUS_DISTANCE_RANGE.1, d.distance_km);
        f.range(prefix, "tower_km", &mut self.tower_km, CLOUD_CUMULONIMBUS_TOWER_RANGE.0, CLOUD_CUMULONIMBUS_TOWER_RANGE.1, d.tower_km);
        f.range(prefix, "anvil", &mut self.anvil, CLOUD_CUMULONIMBUS_ANVIL_RANGE.0, CLOUD_CUMULONIMBUS_ANVIL_RANGE.1, d.anvil);
        f.range(prefix, "anvil_m", &mut self.anvil_m, CLOUD_CUMULONIMBUS_ANVIL_THICKNESS_RANGE.0, CLOUD_CUMULONIMBUS_ANVIL_THICKNESS_RANGE.1, d.anvil_m);
        f.range(prefix, "overshoot", &mut self.overshoot, CLOUD_CUMULONIMBUS_OVERSHOOT_RANGE.0, CLOUD_CUMULONIMBUS_OVERSHOOT_RANGE.1, d.overshoot);
        f.range(prefix, "lean_deg", &mut self.lean_deg, CLOUD_CUMULONIMBUS_LEAN_RANGE.0, CLOUD_CUMULONIMBUS_LEAN_RANGE.1, d.lean_deg);
        f.range(prefix, "puffiness", &mut self.puffiness, CLOUD_CUMULONIMBUS_PUFFINESS_RANGE.0, CLOUD_CUMULONIMBUS_PUFFINESS_RANGE.1, d.puffiness);
        f.range(prefix, "darkness", &mut self.darkness, CLOUD_CUMULONIMBUS_DARKNESS_RANGE.0, CLOUD_CUMULONIMBUS_DARKNESS_RANGE.1, d.darkness);
        f.range(prefix, "rain_shaft", &mut self.rain_shaft, CLOUD_CUMULONIMBUS_RAIN_SHAFT_RANGE.0, CLOUD_CUMULONIMBUS_RAIN_SHAFT_RANGE.1, d.rain_shaft);
        f.range(prefix, "mammatus", &mut self.mammatus, CLOUD_CUMULONIMBUS_MAMMATUS_RANGE.0, CLOUD_CUMULONIMBUS_MAMMATUS_RANGE.1, d.mammatus);
        f.range(prefix, "density", &mut self.density, CLOUD_DENSITY_RANGE.0, CLOUD_DENSITY_RANGE.1, d.density);
    }
}

impl LenticularParams {
    pub(super) fn clamp_fields(&mut self, prefix: &str, f: &mut Fixes) {
        let d = LenticularParams::default();
        f.range(prefix, "base_m", &mut self.base_m, CLOUD_LENTICULAR_BASE_RANGE.0, CLOUD_LENTICULAR_BASE_RANGE.1, d.base_m);
        f.wrap360(prefix, "azimuth_deg", &mut self.azimuth_deg, d.azimuth_deg);
        f.range(prefix, "distance_km", &mut self.distance_km, CLOUD_LENTICULAR_DISTANCE_RANGE.0, CLOUD_LENTICULAR_DISTANCE_RANGE.1, d.distance_km);
        f.range(prefix, "length_km", &mut self.length_km, CLOUD_LENTICULAR_LENGTH_RANGE.0, CLOUD_LENTICULAR_LENGTH_RANGE.1, d.length_km);
        f.range(prefix, "width_km", &mut self.width_km, CLOUD_LENTICULAR_WIDTH_RANGE.0, CLOUD_LENTICULAR_WIDTH_RANGE.1, d.width_km);
        f.range(prefix, "plates", &mut self.plates, CLOUD_LENTICULAR_PLATES_RANGE.0, CLOUD_LENTICULAR_PLATES_RANGE.1, d.plates);
        f.range(prefix, "thickness_m", &mut self.thickness_m, CLOUD_LENTICULAR_THICKNESS_RANGE.0, CLOUD_LENTICULAR_THICKNESS_RANGE.1, d.thickness_m);
        f.range(prefix, "gap_m", &mut self.gap_m, CLOUD_LENTICULAR_GAP_RANGE.0, CLOUD_LENTICULAR_GAP_RANGE.1, d.gap_m);
        f.range(prefix, "smoothness", &mut self.smoothness, CLOUD_LENTICULAR_SMOOTHNESS_RANGE.0, CLOUD_LENTICULAR_SMOOTHNESS_RANGE.1, d.smoothness);
        f.range(prefix, "striations", &mut self.striations, CLOUD_LENTICULAR_STRIATIONS_RANGE.0, CLOUD_LENTICULAR_STRIATIONS_RANGE.1, d.striations);
        f.range(prefix, "density", &mut self.density, CLOUD_DENSITY_RANGE.0, CLOUD_DENSITY_RANGE.1, d.density);
    }
}

impl ClassicLook {
    pub(super) fn clamp_fields(&mut self, prefix: &str, f: &mut Fixes) {
        let d = ClassicLook::default();
        // The older CloudParams' own ranges (`CloudParams::clamp_fields`).
        f.range(prefix, "coverage", &mut self.coverage, 0.0, 1.0, d.coverage);
        f.range(prefix, "sharpness", &mut self.sharpness, 0.0, 1.0, d.sharpness);
        f.range(prefix, "light_m", &mut self.light_m, CLOUD_CLASSIC_LIGHT_RANGE.0, CLOUD_CLASSIC_LIGHT_RANGE.1, d.light_m);
    }
}

clamped_copy!(CloudStackParams, CloudLayerParams, CirrusParams, CirrostratusParams, AltocumulusParams, NimbostratusParams, StratusParams, StratocumulusParams, CumulusParams, CumulonimbusParams, LenticularParams, ClassicLook);

/// The name "+ Cloud" gives a new layer of `genus`: "<Label> N" with the lowest N >= 1 that no layer of `layers`
/// shows (`cloud_display_name`, so an unnamed layer's shown name counts too), in any case, as a name addresses a
/// layer: beside "stratus 1" a Stratus is "Stratus 2".
pub fn next_cloud_name(layers: &[CloudLayerParams], genus: CloudGenus) -> String {
    let shown: Vec<String> = (0..layers.len()).map(|i| cloud_display_name(layers, i)).collect();
    (1..=layers.len() + 1)
        .map(|n| format!("{} {n}", genus.label()))
        .find(|name| !shown.iter().any(|taken| taken.eq_ignore_ascii_case(name)))
        .expect("of layers.len() + 1 names one is free")
}

/// The name a row shows for `layers[index]`: its trimmed name; for one with none, a free "<Label> N" (the names of
/// their own take theirs first, then each unnamed layer of its type, in list order, the next one left); for one of a
/// genus this build does not know, its trimmed genus. "" for an index past the list.
pub fn cloud_display_name(layers: &[CloudLayerParams], index: usize) -> String {
    let Some(layer) = layers.get(index) else {
        return String::new();
    };
    let name = layer.name.trim();
    if !name.is_empty() {
        return name.to_string();
    }
    let Some(genus) = layer.genus() else {
        return layer.genus.trim().to_string();
    };
    let taken: Vec<&str> = layers.iter().map(|other| other.name.trim()).filter(|other| !other.is_empty()).collect();
    let rank = layers[..index].iter().filter(|other| other.name.trim().is_empty() && other.genus() == Some(genus)).count();
    (1usize..)
        .map(|n| format!("{} {n}", genus.label()))
        .filter(|candidate| !taken.contains(&candidate.as_str()))
        .nth(rank)
        .expect("the free names never run out")
}

/// A fresh seed for a new layer at `index` (design §1.5): 1..=CLOUD_SEED_MAX, never 0 (a lifted layer's).
pub fn fresh_cloud_seed(map_seed: u32, index: usize, genus: CloudGenus) -> u32 {
    1 + hash_u32(map_seed ^ index as u32 ^ CLOUD_GENUS_SALT[genus as usize]) % CLOUD_SEED_MAX
}

/// A fresh seed for a new layer that no layer of `layers` holds: the first `fresh_cloud_seed(map_seed, k, genus)`
/// for k = `layers.len()`, `layers.len() + 1`, ... that none of them has. After a delete the list is shorter, so the
/// plain index would repeat a listed layer's seed, and two layers of one type would draw the same clumps in the same
/// place. A pure function of the list: the same list always gives the same seed.
pub fn free_cloud_seed(layers: &[CloudLayerParams], map_seed: u32, genus: CloudGenus) -> u32 {
    (layers.len()..)
        .map(|index| fresh_cloud_seed(map_seed, index, genus))
        .find(|seed| layers.iter().all(|layer| layer.seed != *seed))
        .expect("a list holds finitely many seeds")
}

/// The seed a layer's streams start from (design §1.1): seed 0 is the map's own; any other is hashed with the map
/// seed and the type, never `seed * 16 + k`, which aliases seeds 2^28 apart (volumetric.rs:180).
pub fn cloud_layer_seed(map_seed: u32, seed: u32, genus: CloudGenus) -> u32 {
    if seed == 0 {
        map_seed
    } else {
        hash_u32(map_seed ^ hash_u32(seed.wrapping_add(CLOUD_GENUS_SALT[genus as usize])))
    }
}

/// The older cirrus' seeded angle (clouds.rs:114) as the compass heading its wind blows from: the streaks run along
/// (cos theta, sin theta) in (x, z), which is downwind (-sin a, cos a) for a = atan2(-cos theta, sin theta), wrapped
/// into 0..360. The Clouds entry shows it while `CloudStackParams::wind_deg` is None.
pub fn seeded_wind_deg(map_seed: u32) -> f32 {
    let theta = hash01(0, 0, 0, hash_u32(map_seed ^ 0x5bd1_e995)) * std::f32::consts::TAU;
    wrap_azimuth((-theta.cos()).atan2(theta.sin()).to_degrees())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A layer of `genus` carrying every type block and the classic marker, each at its defaults.
    fn every_block(genus: &str) -> CloudLayerParams {
        CloudLayerParams {
            genus: genus.to_string(),
            cirrus: Some(CirrusParams::default()),
            altocumulus: Some(AltocumulusParams::default()),
            nimbostratus: Some(NimbostratusParams::default()),
            stratus: Some(StratusParams::default()),
            stratocumulus: Some(StratocumulusParams::default()),
            cumulus: Some(CumulusParams::default()),
            classic: Some(ClassicLook::default()),
            ..CloudLayerParams::default()
        }
    }

    /// The layer as clamp() leaves it in a map of its own, and what clamp() reported.
    fn held(layer: &CloudLayerParams) -> (CloudLayerParams, Vec<String>) {
        let mut p = HdriParams::default();
        p.sky.cloud_stack = Some(CloudStackParams { layers: vec![layer.clone()], ..CloudStackParams::default() });
        let report = p.clamp();
        let mut layers = p.sky.cloud_stack.expect("clamp keeps the stack").layers;
        (layers.remove(0), report)
    }

    type BlockField = (&'static str, (f32, f32), fn(&mut CloudLayerParams) -> &mut f32);

    /// Every number of the six blocks but the tops (held after the base, below) and of the classic marker, with the
    /// range clamp() holds it to.
    fn block_fields() -> [BlockField; 49] {
        [
            ("cirrus.base_m", CLOUD_CIRRUS_BASE_RANGE, |l| &mut l.cirrus.as_mut().unwrap().base_m),
            ("cirrus.size_km", CLOUD_CIRRUS_SIZE_RANGE, |l| &mut l.cirrus.as_mut().unwrap().size_km),
            ("cirrus.streak_length", CLOUD_CIRRUS_STREAK_LENGTH_RANGE, |l| &mut l.cirrus.as_mut().unwrap().streak_length),
            ("cirrus.fineness", CLOUD_CIRRUS_FINENESS_RANGE, |l| &mut l.cirrus.as_mut().unwrap().fineness),
            ("cirrus.hooks", CLOUD_CIRRUS_HOOKS_RANGE, |l| &mut l.cirrus.as_mut().unwrap().hooks),
            ("cirrus.fall_streaks", CLOUD_CIRRUS_FALL_STREAKS_RANGE, |l| &mut l.cirrus.as_mut().unwrap().fall_streaks),
            ("cirrus.patchiness", CLOUD_CIRRUS_PATCHINESS_RANGE, |l| &mut l.cirrus.as_mut().unwrap().patchiness),
            ("cirrus.opacity", CLOUD_CIRRUS_OPACITY_RANGE, |l| &mut l.cirrus.as_mut().unwrap().opacity),
            ("altocumulus.base_m", CLOUD_ALTOCUMULUS_BASE_RANGE, |l| &mut l.altocumulus.as_mut().unwrap().base_m),
            ("altocumulus.cell_m", CLOUD_ALTOCUMULUS_CELL_RANGE, |l| &mut l.altocumulus.as_mut().unwrap().cell_m),
            ("altocumulus.regularity", CLOUD_ALTOCUMULUS_REGULARITY_RANGE, |l| &mut l.altocumulus.as_mut().unwrap().regularity),
            ("altocumulus.rows", CLOUD_ALTOCUMULUS_ROWS_RANGE, |l| &mut l.altocumulus.as_mut().unwrap().rows),
            ("altocumulus.turrets", CLOUD_ALTOCUMULUS_TURRETS_RANGE, |l| &mut l.altocumulus.as_mut().unwrap().turrets),
            ("altocumulus.softness", CLOUD_ALTOCUMULUS_SOFTNESS_RANGE, |l| &mut l.altocumulus.as_mut().unwrap().softness),
            ("nimbostratus.base_m", CLOUD_NIMBOSTRATUS_BASE_RANGE, |l| &mut l.nimbostratus.as_mut().unwrap().base_m),
            ("nimbostratus.size_km", CLOUD_NIMBOSTRATUS_SIZE_RANGE, |l| &mut l.nimbostratus.as_mut().unwrap().size_km),
            ("nimbostratus.darkness", CLOUD_NIMBOSTRATUS_DARKNESS_RANGE, |l| &mut l.nimbostratus.as_mut().unwrap().darkness),
            ("nimbostratus.unevenness", CLOUD_NIMBOSTRATUS_UNEVENNESS_RANGE, |l| &mut l.nimbostratus.as_mut().unwrap().unevenness),
            ("nimbostratus.ragged_base", CLOUD_NIMBOSTRATUS_RAGGED_BASE_RANGE, |l| &mut l.nimbostratus.as_mut().unwrap().ragged_base),
            ("nimbostratus.scud", CLOUD_NIMBOSTRATUS_SCUD_RANGE, |l| &mut l.nimbostratus.as_mut().unwrap().scud),
            ("nimbostratus.scud_height_m", CLOUD_NIMBOSTRATUS_SCUD_HEIGHT_RANGE, |l| &mut l.nimbostratus.as_mut().unwrap().scud_height_m),
            ("nimbostratus.rain_curtains", CLOUD_NIMBOSTRATUS_RAIN_CURTAINS_RANGE, |l| &mut l.nimbostratus.as_mut().unwrap().rain_curtains),
            ("nimbostratus.density", CLOUD_DENSITY_RANGE, |l| &mut l.nimbostratus.as_mut().unwrap().density),
            ("stratus.base_m", CLOUD_STRATUS_BASE_RANGE, |l| &mut l.stratus.as_mut().unwrap().base_m),
            ("stratus.size_km", CLOUD_STRATUS_SIZE_RANGE, |l| &mut l.stratus.as_mut().unwrap().size_km),
            ("stratus.density", CLOUD_STRATUS_DENSITY_RANGE, |l| &mut l.stratus.as_mut().unwrap().density),
            ("stratus.mottling", CLOUD_STRATUS_MOTTLING_RANGE, |l| &mut l.stratus.as_mut().unwrap().mottling),
            ("stratus.streaks", CLOUD_STRATUS_STREAKS_RANGE, |l| &mut l.stratus.as_mut().unwrap().streaks),
            ("stratus.ragged_edges", CLOUD_STRATUS_RAGGED_EDGES_RANGE, |l| &mut l.stratus.as_mut().unwrap().ragged_edges),
            ("stratocumulus.base_m", CLOUD_STRATOCUMULUS_BASE_RANGE, |l| &mut l.stratocumulus.as_mut().unwrap().base_m),
            ("stratocumulus.size_km", CLOUD_STRATOCUMULUS_SIZE_RANGE, |l| &mut l.stratocumulus.as_mut().unwrap().size_km),
            ("stratocumulus.softness", CLOUD_STRATOCUMULUS_SOFTNESS_RANGE, |l| &mut l.stratocumulus.as_mut().unwrap().softness),
            ("stratocumulus.shading", CLOUD_STRATOCUMULUS_SHADING_RANGE, |l| &mut l.stratocumulus.as_mut().unwrap().shading),
            ("stratocumulus.billows", CLOUD_STRATOCUMULUS_BILLOWS_RANGE, |l| &mut l.stratocumulus.as_mut().unwrap().billows),
            ("stratocumulus.rolls", CLOUD_STRATOCUMULUS_ROLLS_RANGE, |l| &mut l.stratocumulus.as_mut().unwrap().rolls),
            ("stratocumulus.density", CLOUD_DENSITY_RANGE, |l| &mut l.stratocumulus.as_mut().unwrap().density),
            ("cumulus.base_m", CLOUD_CUMULUS_BASE_RANGE, |l| &mut l.cumulus.as_mut().unwrap().base_m),
            ("cumulus.size_km", CLOUD_CUMULUS_SIZE_RANGE, |l| &mut l.cumulus.as_mut().unwrap().size_km),
            ("cumulus.puffiness", CLOUD_CUMULUS_PUFFINESS_RANGE, |l| &mut l.cumulus.as_mut().unwrap().puffiness),
            ("cumulus.flat_base", CLOUD_CUMULUS_FLAT_BASE_RANGE, |l| &mut l.cumulus.as_mut().unwrap().flat_base),
            ("cumulus.towers", CLOUD_CUMULUS_TOWERS_RANGE, |l| &mut l.cumulus.as_mut().unwrap().towers),
            ("cumulus.clustering", CLOUD_CUMULUS_CLUSTERING_RANGE, |l| &mut l.cumulus.as_mut().unwrap().clustering),
            ("cumulus.streets", CLOUD_CUMULUS_STREETS_RANGE, |l| &mut l.cumulus.as_mut().unwrap().streets),
            ("cumulus.lean", CLOUD_CUMULUS_LEAN_RANGE, |l| &mut l.cumulus.as_mut().unwrap().lean),
            ("cumulus.softness", CLOUD_CUMULUS_SOFTNESS_RANGE, |l| &mut l.cumulus.as_mut().unwrap().softness),
            ("cumulus.density", CLOUD_DENSITY_RANGE, |l| &mut l.cumulus.as_mut().unwrap().density),
            ("classic.coverage", (0.0, 1.0), |l| &mut l.classic.as_mut().unwrap().coverage),
            ("classic.sharpness", (0.0, 1.0), |l| &mut l.classic.as_mut().unwrap().sharpness),
            ("classic.light_m", CLOUD_CLASSIC_LIGHT_RANGE, |l| &mut l.classic.as_mut().unwrap().light_m),
        ]
    }

    /// Design §1.1's clamp rules: every block present is held by its CLOUD_* consts whatever the genus, the tops
    /// after the base, each Option's NaN is None and an infinity the bound, the winds wrap, seeds and names are held,
    /// and the list is never truncated. The defaults are pinned and sit inside their ranges.
    #[test]
    fn a_stack_clamps_by_its_types_ranges() {
        assert_eq!(CirrusParams::default(), CirrusParams { base_m: 9000.0, size_km: 12.0, streak_length: 7.0, fineness: 0.6, hooks: 0.4, fall_streaks: 0.2, patchiness: 0.3, opacity: 0.5 });
        assert_eq!(AltocumulusParams::default(), AltocumulusParams { base_m: 4000.0, cell_m: 400.0, regularity: 0.3, rows: 0.25, turrets: 0.0, softness: 0.7 });
        assert_eq!(NimbostratusParams::default(), NimbostratusParams { base_m: 1000.0, tops_m: 5000.0, size_km: 8.0, darkness: 0.6, unevenness: 0.4, ragged_base: 0.3, scud: 0.3, scud_height_m: 500.0, rain_curtains: 0.5, volume: false, density: 1.0 });
        assert_eq!(StratusParams::default(), StratusParams { base_m: 400.0, size_km: 6.0, density: 0.35, mottling: 0.25, streaks: 0.5, ragged_edges: 0.4 });
        assert_eq!(StratocumulusParams::default(), StratocumulusParams { base_m: 1200.0, tops_m: 1700.0, size_km: 2.5, softness: 0.5, shading: 0.5, billows: 0.5, rolls: 0.3, volume: false, density: 1.0 });
        assert_eq!(CumulusParams::default(), CumulusParams { base_m: 1200.0, tops_m: 2800.0, size_km: 1.5, puffiness: 0.7, flat_base: 0.7, towers: 0.3, clustering: 0.3, streets: 0.0, lean: 0.2, softness: 0.4, volume: true, density: 1.0 });
        assert_eq!(ClassicLook::default(), ClassicLook { coverage: 0.0, sharpness: 0.5, light_m: 2000.0 });
        assert_eq!(CloudStackParams::default(), CloudStackParams { cover: 1.0, wind_deg: None, march_steps: 64, layers: Vec::new() });
        let genera = ["cumulus", "cirrus", "nimbostratus", "stratus", "stratocumulus", "noctilucent"];
        for genus in genera {
            assert!(held(&every_block(genus)).1.is_empty(), "{genus}: the defaults pass clamp unchanged");
        }
        // Every block present is held, whatever the layer's genus, and reported by its path; a base driven over the
        // tops moves them too, reported after it.
        for genus in genera {
            for (name, (lo, hi), field) in block_fields() {
                for (value, want) in [(-1.0e9f32, lo), (1.0e9, hi)] {
                    let mut layer = every_block(genus);
                    *field(&mut layer) = value;
                    // The Scud height is held under the base too (below): the default 1000 m base leaves it 950.
                    let want = if name == "nimbostratus.scud_height_m" { want.min(NimbostratusParams::default().base_m - 50.0) } else { want };
                    let (mut kept, report) = held(&layer);
                    assert_eq!(*field(&mut kept), want, "{genus}: {name} at {value}");
                    assert_eq!(report[0], format!("sky.cloud_stack.layers[0].{name}"), "{genus}: {name} at {value}");
                    assert!(report[1..].iter().all(|path| path.ends_with(".tops_m") || path.ends_with(".scud_height_m")), "{genus}: {name} at {value}: {report:?}");
                }
            }
        }
        // The tops after the base: from the lowest base they reach their whole range; a base moved over them lifts
        // them 200 m clear; a NaN tops lands the type's own thickness over the base.
        type Tops = (&'static str, (f32, f32), (f32, f32), f32, fn(&mut CloudLayerParams) -> (&mut f32, &mut f32));
        let tops: [Tops; 3] = [
            ("nimbostratus", CLOUD_NIMBOSTRATUS_BASE_RANGE, CLOUD_NIMBOSTRATUS_TOPS_RANGE, 4000.0, |l| {
                let b = l.nimbostratus.as_mut().unwrap();
                (&mut b.base_m, &mut b.tops_m)
            }),
            ("stratocumulus", CLOUD_STRATOCUMULUS_BASE_RANGE, CLOUD_STRATOCUMULUS_TOPS_RANGE, 500.0, |l| {
                let b = l.stratocumulus.as_mut().unwrap();
                (&mut b.base_m, &mut b.tops_m)
            }),
            ("cumulus", CLOUD_CUMULUS_BASE_RANGE, CLOUD_CUMULUS_TOPS_RANGE, 1600.0, |l| {
                let b = l.cumulus.as_mut().unwrap();
                (&mut b.base_m, &mut b.tops_m)
            }),
        ];
        for (name, base_range, tops_range, thickness, field) in tops {
            let path = format!("sky.cloud_stack.layers[0].{name}.tops_m");
            assert_eq!(tops_range.0, base_range.0 + CLOUD_MIN_THICKNESS_M, "{name}: the lowest tops clear the lowest base");
            let with = |base: f32, top: f32| {
                let mut layer = every_block("cumulus");
                // The lowest Scud height, which no base moves (it is held after the base, below).
                layer.nimbostratus.as_mut().unwrap().scud_height_m = CLOUD_NIMBOSTRATUS_SCUD_HEIGHT_RANGE.0;
                let (b, t) = field(&mut layer);
                (*b, *t) = (base, top);
                let (mut kept, report) = held(&layer);
                let (_, t) = field(&mut kept);
                (*t, report)
            };
            assert_eq!(with(base_range.0, -1.0e9), (tops_range.0, vec![path.clone()]), "{name}");
            assert_eq!(with(base_range.0, 1.0e9), (tops_range.1, vec![path.clone()]), "{name}");
            assert_eq!(with(5000.0, 3000.0), (5200.0, vec![path.clone()]), "{name}");
            assert_eq!(with(1000.0, f32::NAN), (1000.0 + thickness, vec![path.clone()]), "{name}");
        }
        // The Scud height after the base, as the drawing places the scud: at most 50 m under it, the range's lowest
        // when there is no room (the drawing skips it there), and one in range under the base stays.
        let path = "sky.cloud_stack.layers[0].nimbostratus.scud_height_m".to_string();
        let scud = |base: f32, scud: f32| {
            let layer = CloudLayerParams { nimbostratus: Some(NimbostratusParams { base_m: base, scud_height_m: scud, ..NimbostratusParams::default() }), ..CloudLayerParams::default() };
            let (kept, report) = held(&layer);
            (kept.nimbostratus.unwrap().scud_height_m, report)
        };
        assert_eq!(scud(300.0, 1000.0), (250.0, vec![path.clone()]));
        assert_eq!(scud(3000.0, 1500.0), (1500.0, vec![]));
        assert_eq!(scud(150.0, 400.0), (CLOUD_NIMBOSTRATUS_SCUD_HEIGHT_RANGE.0, vec![path.clone()]));
        assert_eq!(scud(100.0, CLOUD_NIMBOSTRATUS_SCUD_HEIGHT_RANGE.0), (CLOUD_NIMBOSTRATUS_SCUD_HEIGHT_RANGE.0, vec![]));
        assert_eq!(scud(1000.0, f32::NAN), (500.0, vec![path.clone()]));
        assert_eq!(scud(400.0, f32::NAN), (350.0, vec![path.clone()]));
        // Under a base past the top of the tops' range the tops still clear it by 200 m.
        let high = CloudLayerParams { stratocumulus: Some(StratocumulusParams { base_m: 12_000.0, ..StratocumulusParams::default() }), ..CloudLayerParams::default() };
        assert_eq!(held(&high).0.stratocumulus.unwrap().tops_m, 12_200.0);
        // The common rows: each Option is held into its range, Some(NaN) is None (the type's default), and a number in
        // range stays.
        type Common = (&'static str, (f32, f32), fn(&mut CloudLayerParams) -> &mut Option<f32>);
        let common: [Common; 6] = [
            ("cover", CLOUD_COVER_RANGE, |l| &mut l.cover),
            ("drift_km", CLOUD_DRIFT_RANGE, |l| &mut l.drift_km),
            ("brightness", CLOUD_BRIGHTNESS_RANGE, |l| &mut l.brightness),
            ("sky_light", CLOUD_SKY_LIGHT_RANGE, |l| &mut l.sky_light),
            ("lining", CLOUD_LINING_RANGE, |l| &mut l.lining),
            ("shade_below", CLOUD_SHADE_BELOW_RANGE, |l| &mut l.shade_below),
        ];
        for (name, (lo, hi), field) in common {
            let path = format!("sky.cloud_stack.layers[0].{name}");
            for (value, want) in [(-1.0e9f32, Some(lo)), (1.0e9, Some(hi)), (f32::NEG_INFINITY, Some(lo)), (f32::INFINITY, Some(hi)), (f32::NAN, None)] {
                let mut layer = CloudLayerParams::default();
                *field(&mut layer) = Some(value);
                let (mut kept, report) = held(&layer);
                assert_eq!(*field(&mut kept), want, "{name} at {value}");
                assert_eq!(report, [path.clone()], "{name} at {value}");
            }
            let mut layer = CloudLayerParams::default();
            *field(&mut layer) = Some(0.5 * (lo + hi));
            assert!(held(&layer).1.is_empty(), "{name}: a number in range stays");
        }
        // A layer's wind wraps; one that is not a number is the Clouds wind (None).
        for (value, want) in [(370.0f32, Some(10.0f32)), (-30.0, Some(330.0)), (f32::INFINITY, None), (f32::NAN, None)] {
            let (kept, report) = held(&CloudLayerParams { wind_deg: Some(value), ..CloudLayerParams::default() });
            assert_eq!(kept.wind_deg, want, "wind {value}");
            assert_eq!(report, ["sky.cloud_stack.layers[0].wind_deg"]);
        }
        assert!(held(&CloudLayerParams { wind_deg: Some(359.5), ..CloudLayerParams::default() }).1.is_empty());
        // The name keeps 64 characters, as a light's; the seed is held to CLOUD_SEED_MAX.
        let (kept, report) = held(&CloudLayerParams { name: "x".repeat(70), seed: u32::MAX, ..CloudLayerParams::default() });
        assert_eq!((kept.name.chars().count(), kept.seed), (64, CLOUD_SEED_MAX));
        assert_eq!(report, ["sky.cloud_stack.layers[0].name", "sky.cloud_stack.layers[0].seed"]);
        // The stack's own fields, and the list is never truncated.
        let mut p = HdriParams::default();
        p.sky.cloud_stack = Some(CloudStackParams { cover: 1.0e9, wind_deg: Some(-90.0), march_steps: 5, layers: vec![CloudLayerParams::default(); 40] });
        assert_eq!(p.clamp(), ["sky.cloud_stack.cover", "sky.cloud_stack.wind_deg", "sky.cloud_stack.march_steps"]);
        let stack = p.sky.cloud_stack.as_ref().unwrap();
        assert_eq!((stack.cover, stack.wind_deg, stack.march_steps, stack.layers.len()), (1.0, Some(270.0), 16, 40));
        p.sky.cloud_stack.as_mut().unwrap().march_steps = 500;
        assert_eq!(p.clamp(), ["sky.cloud_stack.march_steps"]);
        assert_eq!(p.sky.cloud_stack.as_ref().unwrap().march_steps, 96);
        assert!(p.clamp().is_empty());
        // clamped() and the block accessors read a block as clamp() holds it; a missing block is the type's defaults.
        let wild = CirrusParams { fineness: 3.0, ..CirrusParams::default() };
        assert_eq!(wild.clamped().fineness, 1.0);
        assert_eq!(CloudLayerParams { cirrus: Some(wild), ..CloudLayerParams::default() }.cirrus_params().fineness, 1.0);
        assert_eq!(CloudLayerParams::default().cirrus_params(), CirrusParams::default());
        assert_eq!(CloudLayerParams::default().cumulus_params(), CumulusParams::default());
    }

    /// Design §1.1: a genus this build does not know is kept as written, not reported, and its blocks are still
    /// held; a known one spelled loosely is normalised and reported. The typed view follows the chart. A layer's
    /// species is held as its genus is (the owner's species, 2026-11-01): one its type does not have in this build (a
    /// newer build's, another type's) is kept as written through clamp() and a save, never reported; one of its type
    /// spelled loosely, or by its label, is written as the type's table has it and reported.
    #[test]
    fn an_unknown_genus_is_kept_verbatim() {
        let mut p = HdriParams::default();
        p.sky.cloud_stack = Some(CloudStackParams {
            layers: vec![
                CloudLayerParams { genus: " Noctilucent ".to_string(), cirrus: Some(CirrusParams { fineness: 7.0, ..CirrusParams::default() }), ..CloudLayerParams::default() },
                CloudLayerParams { genus: "  CuMulus".to_string(), species: Some(" Pileus ".to_string()), ..CloudLayerParams::default() },
                CloudLayerParams { genus: "cirrocumulus".to_string(), species: Some("Castellanus".to_string()), ..CloudLayerParams::default() },
                CloudLayerParams { genus: "stratus".to_string(), species: Some("congestus".to_string()), ..CloudLayerParams::default() },
                CloudLayerParams { genus: "stratus".to_string(), species: Some("  Stratus NEBULOSUS".to_string()), ..CloudLayerParams::default() },
                CloudLayerParams { genus: "nimbostratus".to_string(), species: Some("nebulosus".to_string()), ..CloudLayerParams::default() },
            ],
            ..CloudStackParams::default()
        });
        let report = p.clamp();
        let layers = &p.sky.cloud_stack.as_ref().unwrap().layers;
        assert_eq!(layers[0].genus, " Noctilucent ", "a genus this build does not know is kept as written");
        assert_eq!(layers[0].genus(), None);
        assert_eq!(layers[0].cirrus.as_ref().unwrap().fineness, 1.0, "its blocks are still held, whatever the genus");
        assert_eq!(layers[1].genus, "cumulus", "a known one spelled loosely is normalised");
        assert_eq!(layers[2].genus(), Some(CloudGenus::Cirrocumulus));
        assert!(!CloudGenus::Cirrocumulus.is_built() && CloudGenus::Cumulus.is_built());
        // Species: unknown to the type (a newer build's, another type's, a type of one form) kept as written; a known one
        // written as its table has it, even on a type this build does not draw yet.
        let species: Vec<Option<&str>> = layers.iter().map(|layer| layer.species.as_deref()).collect();
        assert_eq!(species, [None, Some(" Pileus "), Some("castellanus"), Some("congestus"), Some("nebulosus"), Some("nebulosus")]);
        assert_eq!(layers[1].species_of(), Some("humilis"), "the rows hold the type's first species, whatever name is kept");
        assert_eq!(
            report,
            ["sky.cloud_stack.layers[0].cirrus.fineness", "sky.cloud_stack.layers[1].genus", "sky.cloud_stack.layers[2].species", "sky.cloud_stack.layers[4].species"]
        );
        assert!(p.clamp().is_empty(), "the unknown genus and the unknown species are never reported");
        // A save and a load keep the unknown species as written, and a layer without one writes none.
        let json = p.to_json();
        assert_eq!(json.matches("\"species\"").count(), 5, "{json}");
        let loaded = HdriParams::from_json(&json).expect("the file loads");
        assert_eq!(loaded.sky.cloud_stack.as_ref().unwrap().layers[1].species.as_deref(), Some(" Pileus "));
        assert_eq!(loaded, p, "the round trip keeps every species");
        assert_eq!(cloud_display_name(&p.sky.cloud_stack.as_ref().unwrap().layers, 0), "Noctilucent", "an unnamed layer of an unknown genus shows its genus");
        // The typed view: of() reads loose spellings and whole names only; the chart's order indexes everything.
        assert_eq!(CloudGenus::of(" STRATUS "), Some(CloudGenus::Stratus));
        assert_eq!(CloudGenus::of("cumulus congestus"), None);
        for (i, genus) in CloudGenus::ALL.into_iter().enumerate() {
            assert_eq!(genus as usize, i);
            assert_eq!(genus.as_str(), CLOUD_GENUS_NAMES[i]);
            assert_eq!(CloudGenus::of(genus.label()), Some(genus));
            assert_eq!(CLOUD_GENUS_SALT[i], 0x85EB_CA6Bu32.wrapping_mul(i as u32 + 1));
        }
        assert_eq!(CLOUD_GENUS_BUILT.map(CloudGenus::label), ["Cirrus", "Cirrostratus", "Altocumulus", "Nimbostratus", "Stratus", "Stratocumulus", "Cumulus", "Cumulonimbus", "Lenticular"]);
        assert_eq!(CLOUD_GENUS_BUILT.map(CloudGenus::default_cover), [0.35, 0.9, 0.6, 1.0, 0.6, 0.6, 0.35, 1.0, 1.0]);
        // A new Stratus is made closed, its first species nebulosus (the owner, 2026-11-01); a saved one with no Cover
        // keeps its 0.6. Every other type is made at its default.
        assert_eq!(CLOUD_GENUS_BUILT.map(CloudGenus::new_cover), [None, None, None, None, Some(1.0), None, None, None, None]);
        // (a Stratus volume 0.6: the volumes design §2.4, a layer's lower forward share)
        assert_eq!(CLOUD_GENUS_BUILT.map(CloudGenus::default_lining), [0.5, 1.0, 0.75, 1.0, 0.6, 0.75, 0.75, 0.75, 0.75]);
        assert_eq!(CLOUD_GENUS_BUILT.map(CloudGenus::default_shade_below), [0.5, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0]);
    }

    /// Design §1.1, §5.5: "<Type> N" with the lowest free N (an unnamed layer's shown name counts), fresh seeds in
    /// 1..=CLOUD_SEED_MAX that no listed layer holds, and a layer's streams by the map seed, its seed and its type.
    #[test]
    fn new_layers_get_the_lowest_free_name_and_a_fresh_seed() {
        let named = |genus: CloudGenus, name: &str| CloudLayerParams { genus: genus.as_str().to_string(), name: name.to_string(), ..CloudLayerParams::default() };
        let layers = vec![
            named(CloudGenus::Cumulus, "Cumulus 1"),
            named(CloudGenus::Cumulus, ""),
            named(CloudGenus::Cumulus, " Cumulus 4 "),
            named(CloudGenus::Cirrus, ""),
            named(CloudGenus::Cirrus, ""),
        ];
        let shown: Vec<String> = (0..layers.len()).map(|i| cloud_display_name(&layers, i)).collect();
        assert_eq!(shown, ["Cumulus 1", "Cumulus 2", "Cumulus 4", "Cirrus 1", "Cirrus 2"]);
        assert_eq!(cloud_display_name(&layers, 99), "", "an index past the list names nothing");
        assert_eq!(next_cloud_name(&layers, CloudGenus::Cumulus), "Cumulus 3");
        assert_eq!(next_cloud_name(&layers, CloudGenus::Cirrus), "Cirrus 3");
        assert_eq!(next_cloud_name(&layers, CloudGenus::Stratus), "Stratus 1");
        assert_eq!(next_cloud_name(&[], CloudGenus::Nimbostratus), "Nimbostratus 1");
        assert_eq!(next_cloud_name(&[named(CloudGenus::Stratus, "stratus 1")], CloudGenus::Stratus), "Stratus 2", "a name in another case is taken");
        assert_eq!(next_cloud_name(&[named(CloudGenus::Cirrus, " CIRRUS 1 ")], CloudGenus::Cirrus), "Cirrus 2");
        let fresh = CloudLayerParams::new(CloudGenus::Cumulus, &layers, 7);
        assert_eq!(
            fresh,
            CloudLayerParams {
                name: "Cumulus 3".to_string(),
                genus: "cumulus".to_string(),
                seed: fresh_cloud_seed(7, 5, CloudGenus::Cumulus),
                cumulus: Some(CumulusParams::default()),
                ..CloudLayerParams::default()
            }
        );
        assert!(fresh.enabled && fresh.cover.is_none() && fresh.classic.is_none());
        for genus in CLOUD_GENUS_BUILT {
            let layer = CloudLayerParams::new(genus, &[], 1);
            let blocks = [
                layer.cirrus.is_some(),
                layer.cirrostratus.is_some(),
                layer.altocumulus.is_some(),
                layer.nimbostratus.is_some(),
                layer.stratus.is_some(),
                layer.stratocumulus.is_some(),
                layer.cumulus.is_some(),
                layer.cumulonimbus.is_some(),
                layer.lenticular.is_some(),
            ];
            assert_eq!(blocks.iter().filter(|&&b| b).count(), 1, "{genus:?}: its own block alone");
            assert_eq!(layer.genus(), Some(genus));
            let base = match genus {
                CloudGenus::Cirrus => 9000.0,
                CloudGenus::Cirrostratus => 8500.0,
                CloudGenus::Altocumulus => 4000.0,
                CloudGenus::Nimbostratus => 1000.0,
                CloudGenus::Stratus => 400.0,
                CloudGenus::Cumulonimbus => 1000.0,
                CloudGenus::Lenticular => 5000.0,
                _ => 1200.0,
            };
            assert_eq!(layer.base_m(), base, "{genus:?}");
        }
        assert_eq!(CloudLayerParams::new(CloudGenus::Cirrocumulus, &[], 1).base_m(), 0.0, "a type not built yet has no block");
        assert_eq!(named(CloudGenus::Cumulus, "").base_m(), 1200.0, "no block: the type's defaults");
        assert_eq!(CloudLayerParams { genus: "noctilucent".to_string(), ..CloudLayerParams::default() }.base_m(), 0.0);
        // Fresh seeds: never 0, at most CLOUD_SEED_MAX, and a new one for each index and type.
        let mut seen = HashSet::new();
        for genus in CLOUD_GENUS_BUILT {
            for index in 0..8 {
                for map_seed in [0u32, 1, 42, u32::MAX] {
                    let seed = fresh_cloud_seed(map_seed, index, genus);
                    assert!((1..=CLOUD_SEED_MAX).contains(&seed), "{map_seed} {index} {genus:?}: {seed}");
                    if map_seed == 42 {
                        assert!(seen.insert(seed), "{index} {genus:?}: {seed} again");
                    }
                }
            }
        }
        // A delete brings no listed seed back: two layers of one type, the first deleted, then a third added.
        let first = CloudLayerParams::new(CloudGenus::Cumulus, &[], 7);
        let second = CloudLayerParams::new(CloudGenus::Cumulus, &[first], 7);
        let listed = [second];
        let third = CloudLayerParams::new(CloudGenus::Cumulus, &listed, 7);
        assert!(listed.iter().all(|layer| layer.seed != third.seed), "after a delete the new layer takes a listed seed, {}", third.seed);
        // A layer's streams: seed 0 is the map's own; another is hashed with the map seed and the type, so seeds 2^28
        // apart do not alias (volumetric.rs:180's seed * 16 + k would).
        assert_eq!(cloud_layer_seed(42, 0, CloudGenus::Cumulus), 42);
        assert_ne!(cloud_layer_seed(42, 1, CloudGenus::Cumulus), cloud_layer_seed(42, 1 + (1 << 28), CloudGenus::Cumulus));
        assert_ne!(cloud_layer_seed(42, 1, CloudGenus::Cumulus), cloud_layer_seed(42, 1, CloudGenus::Cirrus));
        assert_ne!(cloud_layer_seed(42, 1, CloudGenus::Cumulus), cloud_layer_seed(43, 1, CloudGenus::Cumulus));
        // The seeded wind: the older cirrus' angle (clouds.rs:114) as the heading it blows from, downwind (-sin a, cos a).
        for map_seed in [0u32, 1, 7, 42, 0xdead_beef] {
            let theta = hash01(0, 0, 0, hash_u32(map_seed ^ 0x5bd1_e995)) * std::f32::consts::TAU;
            let from = seeded_wind_deg(map_seed);
            assert!((0.0..360.0).contains(&from), "{map_seed}: {from}");
            let (along_x, along_z) = (-from.to_radians().sin(), from.to_radians().cos());
            assert!((along_x - theta.cos()).abs() < 1.0e-5 && (along_z - theta.sin()).abs() < 1.0e-5, "{map_seed}: {from} against {theta}");
        }
        // What a layer costs (design §7 estimates about 300 bytes a layer), for the commit message.
        let json = CloudLayerParams::new(CloudGenus::Cumulus, &[], 1).serialize_json().len();
        println!("a cloud layer: {} bytes in memory, {json} bytes of JSON with one block", std::mem::size_of::<CloudLayerParams>());
    }

    /// Design §5.4: a Type change keeps every block and makes the new type's, a default Cover and a default name
    /// follow the type, and `classic` goes; picking the type a layer has changes nothing.
    #[test]
    fn a_type_change_keeps_every_block() {
        let mut layer = CloudLayerParams::new(CloudGenus::Cumulus, &[], 1);
        layer.cumulus.as_mut().unwrap().puffiness = 0.9;
        layer.cover = Some(0.5);
        let others = [layer.clone()];
        layer.set_genus(CloudGenus::Cirrus, &others);
        assert_eq!(layer.genus, "cirrus");
        assert_eq!(layer.cirrus, Some(CirrusParams::default()), "the new type's block is made");
        assert_eq!(layer.cumulus.as_ref().unwrap().puffiness, 0.9, "the old one is kept");
        assert_eq!(layer.cover, Some(0.5), "a Cover off the old type's default stays");
        assert_eq!(layer.name, "Cirrus 1", "a default name follows the type");
        layer.cirrus.as_mut().unwrap().fineness = 0.1;
        layer.set_genus(CloudGenus::Cumulus, &[]);
        assert_eq!((layer.cumulus.as_ref().unwrap().puffiness, layer.cirrus.as_ref().unwrap().fineness), (0.9, 0.1), "switching back finds both");
        assert_eq!(layer.name, "Cumulus 1");
        // A default Cover follows the type: None, the old type's own default, or the Cover the old type makes a new layer
        // with (a new Stratus' 1), becomes the new type's: None, its default, but a Stratus' 1 (`new_cover`). A Cover
        // of one's own stays.
        let mut made = CloudLayerParams::new(CloudGenus::Stratus, &[], 1);
        assert_eq!(made.cover, Some(1.0), "a new Stratus is made closed");
        made.set_genus(CloudGenus::Cirrus, &[]);
        assert_eq!(made.cover, None, "a new Stratus turned Cirrus takes the Cirrus' Cover, not a closed sky");
        let mut at_default = CloudLayerParams { cover: Some(CloudGenus::Stratus.default_cover()), ..CloudLayerParams::new(CloudGenus::Stratus, &[], 1) };
        at_default.set_genus(CloudGenus::Nimbostratus, &[]);
        assert_eq!(at_default.cover, None);
        let mut into = CloudLayerParams::new(CloudGenus::Cumulus, &[], 1);
        into.set_genus(CloudGenus::Stratus, &[]);
        assert_eq!(into.cover, Some(1.0), "a Type change into a Stratus makes it closed, as a new one");
        let mut own_cover = CloudLayerParams { cover: Some(0.5), ..CloudLayerParams::new(CloudGenus::Cumulus, &[], 1) };
        own_cover.set_genus(CloudGenus::Stratus, &[]);
        assert_eq!(own_cover.cover, Some(0.5));
        own_cover.set_genus(CloudGenus::Cumulus, &[]);
        assert_eq!(own_cover.cover, Some(0.5));
        // A name of one's own stays, the classic marker goes, and the same type changes nothing.
        let mut own = CloudLayerParams { name: "Morning haze".to_string(), classic: Some(ClassicLook::default()), ..CloudLayerParams::new(CloudGenus::Stratocumulus, &[], 1) };
        let before = own.clone();
        own.set_genus(CloudGenus::Stratocumulus, &[]);
        assert_eq!(own, before, "picking the type it has changes nothing");
        own.set_genus(CloudGenus::Cumulus, &[]);
        assert_eq!((own.name.as_str(), own.classic.is_none()), ("Morning haze", true));
        assert_eq!(own.stratocumulus, before.stratocumulus);
        assert_eq!(own.cumulus, Some(CumulusParams::default()));
        // "<Old type> N" is a default name; anything else is the user's.
        let cirrus_1 = CloudLayerParams { name: "Cirrus 1".to_string(), ..CloudLayerParams::new(CloudGenus::Cirrus, &[], 1) };
        let mut numbered = CloudLayerParams { name: "Stratus 3".to_string(), ..CloudLayerParams::new(CloudGenus::Stratus, &[], 1) };
        numbered.set_genus(CloudGenus::Cirrus, &[cirrus_1]);
        assert_eq!(numbered.name, "Cirrus 2");
        let mut not_default = CloudLayerParams { name: "Stratus 3b".to_string(), ..CloudLayerParams::new(CloudGenus::Stratus, &[], 1) };
        not_default.set_genus(CloudGenus::Cirrus, &[]);
        assert_eq!(not_default.name, "Stratus 3b");
        // A layer of an unknown genus takes the type, its block and a default name.
        let mut unknown = CloudLayerParams { genus: "noctilucent".to_string(), ..CloudLayerParams::default() };
        unknown.set_genus(CloudGenus::Cumulus, &[]);
        assert_eq!((unknown.genus.as_str(), unknown.name.as_str(), unknown.cumulus.clone()), ("cumulus", "Cumulus 1", Some(CumulusParams::default())));
    }

    // ---- merge_json and the files (design §1.4, §1.5) ------------------------------------------------------------

    /// SkyParams as makepad commercial 4b4d82c2 has it, before the cloud layers: the shape an older build reads.
    #[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
    struct OlderSky {
        sun: SunParams,
        atmosphere: AtmosphereParams,
        sun_disc: SunDiscParams,
        clouds: CloudParams,
        night: NightParams,
        volumetric: Option<VolumetricParams>,
        weather: Option<WeatherParams>,
    }

    /// Design §1.4: a map without layers writes no stack, loads with none (loading lifts nothing) and saves again
    /// byte for byte: every preset and every clouds baseline case.
    #[test]
    fn old_files_load_and_resave_unchanged() {
        use crate::presets::{preset, PRESET_NAMES};
        let mut maps: Vec<(String, HdriParams)> = PRESET_NAMES.iter().map(|n| (n.to_string(), preset(n).unwrap())).collect();
        maps.extend(crate::cloud_baseline::clouds_baseline_cases().into_iter().map(|(name, p, _)| (name, p)));
        for (name, p) in &maps {
            let json = p.to_json();
            assert!(!json.contains("cloud_stack"), "{name}: a map without layers writes no stack");
            let loaded = HdriParams::from_json(&json).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(&loaded, p, "{name}");
            assert!(loaded.sky.cloud_stack.is_none(), "{name}: loading lifts nothing");
            assert_eq!(loaded.to_json(), json, "{name}: saved again as it was");
        }
    }

    /// Design §1.4: an older build loads a new file (the stack is a key it skips, sky.clouds is written at its
    /// defaults), draws no layer clouds but still folds its kind's floors, so a new Rain file shows a closed deck
    /// there; its re-save drops the stack.
    #[test]
    fn a_new_file_loads_in_an_older_build() {
        let mut p = crate::presets::preset("Clear noon").unwrap();
        p.sky.weather = Some(WeatherParams { kind: "rain".to_string(), ..WeatherParams::default() });
        let rain = CloudLayerParams { cover: Some(1.0), brightness: Some(0.7), ..CloudLayerParams::new(CloudGenus::Nimbostratus, &[], p.seed) };
        p.sky.cloud_stack = Some(CloudStackParams { layers: vec![rain], ..CloudStackParams::default() });
        p.sky.clouds = CloudParams::default();
        let json = p.to_json();
        assert!(json.contains("\"cloud_stack\""));
        let sky_json = JsonValue::deserialize_json(&json).unwrap().get("sky").unwrap().serialize_json();
        let older = OlderSky::deserialize_json_lenient(&sky_json).expect("an older build loads a new file");
        assert_eq!(older.clouds, CloudParams::default(), "sky.clouds is written at its defaults");
        assert_eq!(older.weather, p.sky.weather);
        let as_older = SkyParams {
            sun: older.sun.clone(),
            atmosphere: older.atmosphere.clone(),
            sun_disc: older.sun_disc.clone(),
            clouds: older.clouds.clone(),
            night: older.night.clone(),
            volumetric: older.volumetric.clone(),
            weather: older.weather.clone(),
            cloud_stack: None,
        };
        let folded = crate::weather::weathered_sky(&as_older);
        assert_eq!(folded.clouds.coverage, 1.0, "the Rain kind still closes the older build's deck");
        assert!(folded.clouds.sharpness <= 0.2);
        assert!(!older.serialize_json().contains("cloud_stack"), "a re-save there drops the stack");
    }

    /// Design §1.1: a layer of a genus this build does not know loads, keeps its name, genus, cover, seed, wind and
    /// eye through a re-save, and loses only its unknown block, as any unknown key; a known type not built yet the same.
    #[test]
    fn a_newer_genus_survives_a_resave() {
        let stack = r#"{"cover": 0.8, "march_steps": 64, "layers": [
            {"enabled": false, "name": "Lens 1", "genus": "Noctilucent", "seed": 77, "cover": 0.4, "wind_deg": 120, "noctilucent": {"glow": 0.5}},
            {"enabled": true, "name": "", "genus": "cirrocumulus", "seed": 5, "cirrocumulus": {"grain_m": 60}}]}"#;
        let mut tree = JsonValue::deserialize_json(&HdriParams::default().to_json()).unwrap();
        tree.get_mut("sky").unwrap().as_object_mut().unwrap().insert("cloud_stack".to_string(), JsonValue::deserialize_json(stack).unwrap());
        let p = HdriParams::from_json(&tree.serialize_json()).expect("a file from a newer build loads");
        let layers = &p.sky.cloud_stack.as_ref().unwrap().layers;
        assert_eq!(layers.len(), 2);
        let lens = &layers[0];
        assert_eq!((lens.enabled, lens.name.as_str(), lens.genus.as_str(), lens.seed, lens.cover, lens.wind_deg), (false, "Lens 1", "Noctilucent", 77, Some(0.4), Some(120.0)));
        assert_eq!(lens.genus(), None);
        assert_eq!(layers[1].genus(), Some(CloudGenus::Cirrocumulus));
        assert!(p.clone().clamp().is_empty(), "the genus is kept verbatim and not reported");
        let resaved = p.to_json();
        assert!(resaved.contains("Noctilucent"));
        assert!(!resaved.contains("\"glow\"") && !resaved.contains("\"grain_m\""), "the unknown blocks are dropped, as any unknown key");
        assert_eq!(HdriParams::from_json(&resaved), Ok(p.clone()), "the rest survives the re-save");
        assert_eq!(cloud_display_name(layers, 1), "Cirrocumulus 1");
    }

    /// Design §1.5: template_for knows the stack, its layers and every block, so a patch naming one field of a block
    /// is a complete block.
    #[test]
    fn every_cloud_block_takes_a_partial_patch() {
        let layer_of = |patch: &str| {
            let (p, report) = HdriParams::default().merge_json(patch).unwrap_or_else(|e| panic!("{patch}: {e}"));
            assert!(report.is_empty(), "{patch}: {report:?}");
            let mut layers = p.sky.cloud_stack.expect("the patch made a stack").layers;
            assert_eq!(layers.len(), 1);
            layers.remove(0)
        };
        let cirrus = layer_of(r#"{"sky": {"cloud_stack": {"layers": [{"genus": "cirrus", "name": "x", "seed": 3, "cirrus": {"fineness": 0.9}}]}}}"#);
        assert_eq!(cirrus.cirrus, Some(CirrusParams { fineness: 0.9, ..CirrusParams::default() }));
        let nimbostratus = layer_of(r#"{"sky": {"cloud_stack": {"layers": [{"genus": "nimbostratus", "name": "x", "seed": 3, "nimbostratus": {"scud": 0.8}}]}}}"#);
        assert_eq!(nimbostratus.nimbostratus, Some(NimbostratusParams { scud: 0.8, ..NimbostratusParams::default() }));
        let stratus = layer_of(r#"{"sky": {"cloud_stack": {"layers": [{"genus": "stratus", "name": "x", "seed": 3, "stratus": {"mottling": 0.7}}]}}}"#);
        assert_eq!(stratus.stratus, Some(StratusParams { mottling: 0.7, ..StratusParams::default() }));
        let stratocumulus = layer_of(r#"{"sky": {"cloud_stack": {"layers": [{"genus": "stratocumulus", "name": "x", "seed": 3, "stratocumulus": {"rolls": 0.9}}]}}}"#);
        assert_eq!(stratocumulus.stratocumulus, Some(StratocumulusParams { rolls: 0.9, ..StratocumulusParams::default() }));
        let cumulus = layer_of(r#"{"sky": {"cloud_stack": {"layers": [{"genus": "cumulus", "name": "x", "seed": 3, "cumulus": {"streets": 0.6}}]}}}"#);
        assert_eq!(cumulus.cumulus, Some(CumulusParams { streets: 0.6, ..CumulusParams::default() }));
        let classic = layer_of(r#"{"sky": {"cloud_stack": {"layers": [{"genus": "stratocumulus", "name": "x", "seed": 3, "classic": {"coverage": 0.3}}]}}}"#);
        assert_eq!(classic.classic, Some(ClassicLook { coverage: 0.3, ..ClassicLook::default() }));
        // A genus alone is a complete layer, its block read as the type's defaults; a Cover alone a complete stack.
        let bare = layer_of(r#"{"sky": {"cloud_stack": {"layers": [{"genus": "cumulus", "name": "x", "seed": 3}]}}}"#);
        assert_eq!(bare, CloudLayerParams { name: "x".to_string(), seed: 3, ..CloudLayerParams::default() });
        assert_eq!(bare.cumulus_params(), CumulusParams::default());
        let (p, report) = HdriParams::default().merge_json(r#"{"sky": {"cloud_stack": {"cover": 0.4}}}"#).unwrap();
        assert_eq!(p.sky.cloud_stack, Some(CloudStackParams { cover: 0.4, ..CloudStackParams::default() }));
        assert!(report.is_empty(), "{report:?}");
        // The stack's steps and a layer's seed round, as every integer field does.
        let (p, report) = HdriParams::default().merge_json(r#"{"sky": {"cloud_stack": {"march_steps": 40.4, "layers": [{"genus": "cumulus", "name": "x", "seed": 2.6}]}}}"#).unwrap();
        let stack = p.sky.cloud_stack.unwrap();
        assert_eq!((stack.march_steps, stack.layers[0].seed), (40, 3));
        assert_eq!(report, ["sky.cloud_stack.layers[0].seed", "sky.cloud_stack.march_steps"]);
    }

    /// Design §1.5: a layer a patched list adds without a name gets "<Type> N" in list order (the names given count),
    /// without a seed a fresh one no other layer holds, never 0; a given name or seed stays, and a genus this build
    /// does not know keeps the template's.
    #[test]
    fn a_patched_layer_gets_a_name_and_a_fresh_seed() {
        let base = HdriParams { seed: 42, ..HdriParams::default() };
        let patch = r#"{"sky": {"cloud_stack": {"layers": [{"genus": "cirrus"}, {"genus": "cumulus", "seed": 5}, {"genus": "cirrus", "name": "High"}, {"genus": "cirrus"}, {"genus": "noctilucent"}, {"genus": "cumulus", "seed": 2.6}]}}}"#;
        let (p, report) = base.merge_json(patch).unwrap();
        let layers = &p.sky.cloud_stack.as_ref().unwrap().layers;
        let names: Vec<&str> = layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Cirrus 1", "Cumulus 1", "High", "Cirrus 2", "", "Cumulus 2"]);
        assert_eq!(layers[0].seed, fresh_cloud_seed(42, 0, CloudGenus::Cirrus));
        assert_eq!(layers[1].seed, 5, "a given seed stays");
        assert_eq!(layers[2].seed, fresh_cloud_seed(42, 2, CloudGenus::Cirrus));
        assert_eq!(layers[3].seed, fresh_cloud_seed(42, 3, CloudGenus::Cirrus));
        assert_eq!((layers[4].name.as_str(), layers[4].seed), ("", 0), "a genus this build does not know keeps the template's");
        assert_eq!(layers[5].seed, 3, "a seed is rounded like any integer field");
        assert!([0usize, 2, 3].iter().all(|&i| (1..=CLOUD_SEED_MAX).contains(&layers[i].seed)));
        assert_eq!(report, ["sky.cloud_stack.layers[5].seed"]);
        // A kept layer listed beside a new one keeps its seed, and the new one never takes it, even where its own
        // index would give it: this Cumulus was made second (index 1's seed) and the first one was deleted.
        let kept = CloudLayerParams::new(CloudGenus::Cumulus, &[CloudLayerParams::new(CloudGenus::Cumulus, &[], 42)], 42);
        let patch = format!(r#"{{"sky": {{"cloud_stack": {{"layers": [{{"genus": "cumulus", "name": "{}", "seed": {}}}, {{"genus": "cumulus"}}]}}}}}}"#, kept.name, kept.seed);
        let layers = base.merge_json(&patch).unwrap().0.sky.cloud_stack.unwrap().layers;
        assert_eq!(
            (kept.seed, layers[0].seed, layers[1].seed),
            (fresh_cloud_seed(42, 1, CloudGenus::Cumulus), kept.seed, fresh_cloud_seed(42, 2, CloudGenus::Cumulus)),
            "index 1's seed is the kept layer's, so the new layer takes the next one no layer holds"
        );
    }

    /// Design §1.5: on a map with layers a patch to the older deck or volume is put back and said so, while the rest
    /// of the patch applies; on a map without layers the same patch edits the older block, as today.
    #[test]
    fn a_legacy_patch_on_layers_is_reported() {
        let none_map = crate::presets::preset("Golden hour").unwrap();
        let mut stacked = none_map.clone();
        stacked.sky.cloud_stack = Some(CloudStackParams { layers: vec![CloudLayerParams::new(CloudGenus::Cumulus, &[], stacked.seed)], ..CloudStackParams::default() });
        stacked.sky.clouds = CloudParams::default();
        let patch = r#"{"sky": {"clouds": {"coverage": 0.9, "cirrus": 0.2, "fluffiness": 1}, "volumetric": {"density": 2}, "atmosphere": {"haze": 2}}}"#;
        let (p, report) = stacked.merge_json(patch).unwrap();
        assert_eq!(p.sky.clouds, stacked.sky.clouds, "the older deck is put back");
        assert_eq!(p.sky.volumetric, None, "and the older volume");
        assert_eq!(p.sky.atmosphere.haze, 2.0, "the rest of the patch applies");
        assert_eq!(p.sky.cloud_stack, stacked.sky.cloud_stack);
        assert_eq!(
            report,
            [
                "ignored: sky.clouds.fluffiness",
                "ignored: sky.clouds.cirrus (the clouds are layers: hdri.set_clouds)",
                "ignored: sky.clouds.coverage (the clouds are layers: hdri.set_clouds)",
                "ignored: sky.volumetric (the clouds are layers: hdri.set_clouds)",
            ]
        );
        let (q, report) = none_map.merge_json(patch).unwrap();
        assert_eq!((q.sky.clouds.coverage, q.sky.clouds.cirrus), (0.9, 0.2));
        assert_eq!(q.sky.volumetric, Some(VolumetricParams { density: 2.0, ..VolumetricParams::default() }));
        assert_eq!(report, ["ignored: sky.clouds.fluffiness"]);
        // A rounding in a put-back block is not a clamp: only the put-back is said. On an older map it is a clamp.
        let steps = r#"{"sky": {"volumetric": {"steps": 40.5}}}"#;
        let (p, report) = stacked.merge_json(steps).unwrap();
        assert_eq!(p.sky.volumetric, None);
        assert_eq!(report, ["ignored: sky.volumetric (the clouds are layers: hdri.set_clouds)"]);
        let (_, report) = none_map.merge_json(steps).unwrap();
        assert_eq!(report, ["sky.volumetric.steps"]);
    }
}
