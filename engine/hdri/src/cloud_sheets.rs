//! The Sheet class (design §2.1, §4.3): a cloud on a plane at its height, met on the curved Earth (volumetric.rs
//! `shell_distance`), whose 2D recipe gives a noise n in [0, 1] in the layer's wind frame. The cover remap
//! (cloud_cover.rs) turns n into a density, the family into an opacity, and for a heap a self-shadow gives its depth
//! toward the sun. Cirrus (ice), Stratocumulus and the Cumulus' sheet (heap), which stands where its shell will be.
//!
//! - **Wind frame.** (along, across) of the plane in the layer's cells: along is downwind (a wind from a blows along
//!   (-sin a, cos a)), and Drift moves the pattern drift_km x 1000 m downwind.
//! - **Distance LOD**: the fine octaves fade out 4 to 16 cells from the point overhead, as clouds.rs's `detail_at`,
//!   and the threshold blends the fine and the broad octave counts' tables by the same weight. A Cirrus' tufts and
//!   hairs fade into its streams and their mean over the nearest stretch of that, by one weight its threshold blends by
//!   too.
//! - **Horizon.** The alpha is the cover times smoothstep(0, 0.12, sin el): nothing at and below the horizon.

use makepad_draw::*;
use crate::cloud_cover::{fingerprint, quantile_set, CoverRemap, MEASURE_SEED};
use crate::cloud_light::{self, CoverSample, EntryLight, Shape};
use crate::clouds::deck_opacity;
use crate::noise::{billow2, fbm2, hash_u32, ridged2, value2};
use crate::volumetric::shell_distance;
use crate::{cloud_layer_seed, CloudGenus, CloudLayerParams};

/// sin(elevation) at which a sheet reaches its full alpha (clouds.rs's HORIZON_FADE).
const HORIZON_FADE: f32 = 0.12;
/// The fine octaves fade out between these distances from the point overhead, in cells (clouds.rs).
const DETAIL_FADE_START: f32 = 4.0;
const DETAIL_FADE_END: f32 = 16.0;
/// Octaves: the Cirrus' as the older veil's, the heaps' as the older deck's (clouds.rs).
const CIRRUS_OCTAVES: u32 = 4;
const CIRRUS_BROAD_OCTAVES: u32 = 2;
const DECK_OCTAVES: u32 = 6;
const DECK_BROAD_OCTAVES: u32 = 3;
/// The Stratocumulus' fine octaves: one fewer than the deck's, its cells and lumps carry the detail.
const SC_OCTAVES: u32 = 5;
/// sqrt(0.6): with Streak length 6.7 the Cirrus' frame is the older veil's 0.3 along and 2.0 across.
const STREAK_SCALE: f32 = 0.774_596_7;
/// The Cirrus' edge width: the older veil's band (clouds.rs:277).
const CIRRUS_EDGE: f32 = 0.2;
/// Fall streaks' shear along the wind, in the stretched frame's units.
const FALL_SHEAR: f32 = 2.5;
/// The Cirrus' streams, where its faint veil lies and its bundles gather: their frequency along and across the wind in
/// the stretched, bent frame (at the default Size and Streak length a stream is some 20 km long and 3 km wide).
const TUFT_ALONG: f32 = 2.0;
const TUFT_ACROSS: f32 = 2.0;
/// The body from which a patch starts and where it is full: below it the sky is clear.
const TUFT_FROM: f32 = 0.4;
const TUFT_TO: f32 = 0.95;
/// The streams break along their length: a noise BREAK_ALONG cycles along a unit and BREAK_ACROSS across, smoothstepped
/// from BREAK_FROM to BREAK_TO, scales them from BREAK_FLOOR to all (gaps and denser stretches every few km).
const BREAK_ALONG: f32 = 8.0;
const BREAK_ACROSS: f32 = 3.0;
const BREAK_FROM: f32 = 0.3;
const BREAK_TO: f32 = 0.7;
const BREAK_FLOOR: f32 = 0.15;
/// How much more a Fall streaks of 1 lengthens the bundles.
const FALL_LENGTH: f32 = 0.6;
/// The gather: a broad noise (GATHER_FREQ cycles a cell, smoothstepped from GATHER_FROM to GATHER_TO) that gathers the
/// streams over tens of km, and the share of the noise it keeps where it is lowest (open blue).
const GATHER_FREQ: f32 = 0.45;
const GATHER_FROM: f32 = 0.25;
const GATHER_TO: f32 = 0.75;
const GATHER_FLOOR: f32 = 0.3;
/// The heaps' added octaves: billow2's two-octave mean, and how much Puffiness adds.
const BILLOW_MEAN: f32 = 0.38;
const PUFF_AMOUNT: f32 = 0.4;
/// The Cumulus' clumps: a billowed octave at CU_CLUMP_FREQ cycles a cell (at the default Size heaps some 1.5 km
/// across), its share of the noise over the broad field, and the frequency of Puffiness' lumps.
const CU_CLUMP_FREQ: f32 = 1.0;
const CU_CLUMP_SHARE: f32 = 0.6;
const PUFF_FREQ: f32 = 3.5;
/// The Stratocumulus' cells: a billowed octave at SC_CELL_FREQ cycles a cell (at the default Size cells some 1 km
/// across), its share of the noise over the broad field that gathers them, and its lumps' frequency and how much they
/// add per unit of Billows.
const SC_CELL_FREQ: f32 = 2.5;
const SC_CELL_SHARE: f32 = 0.55;
const LUMP_FREQ: f32 = 6.0;
const LUMP_AMOUNT: f32 = 0.5;
/// How much of Softness' edge band a Stratocumulus takes: its cells are crisper than a heap of cumulus.
const SC_SOFTNESS: f32 = 0.5;
/// How much darker a heap's thick body is than its thin edge, as self-shadow depth at Shading 0.5, and over how much
/// of the noise above its threshold it thickens: from below its cells are grey in the middle and bright at the rims.
const BODY_DEPTH: f32 = 4.0;
const BODY_SPAN: f32 = 0.25;
/// The share of the dome a Stratocumulus' underside keeps under any depth, and how fast the rest dims per unit of its
/// self-shadow depth.
const UNDER_FLOOR: f32 = 0.35;
const UNDER_PER_DEPTH: f32 = 0.25;
/// How deep Rolls and Streets cut the troughs between their rows.
const ROLL_DEPTH: f32 = 0.6;
const STREET_DEPTH: f32 = 0.7;
/// The heap's self-shadow, as the older deck's (clouds.rs): the reach per unit of tan(sun zenith) in cells, its cap,
/// the density that dims to e^-3, the octaves and the steps (released; held takes 2 of twice the length).
const SHADOW_REACH: f32 = 0.35;
const SHADOW_MAX_TAN: f32 = 4.0;
const SHADOW_DENSITY: f32 = 3.0;
const SHADOW_OCTAVES: u32 = 4;
const SHADOW_STEPS: usize = 4;
/// Shading's value that is the older deck's self-shadow, and the Cumulus' (it has no Shading row).
const DEFAULT_SHADING: f32 = 0.5;
/// The Cumulus' optical thickness at Towers 0 (the older deck's CUMULUS_THICKNESS); Flat base's grey band, as deep as
/// FLAT_BASE_DEPTH x Flat base from the density FLAT_BASE_CORE in and reaching FLAT_BASE_REACH x Flat base further out
/// (a flatter base is a wider grey); and how far Lean shears one shadow step downwind, in cells.
const CUMULUS_THICKNESS: f32 = 5.0;
const FLAT_BASE_DEPTH: f32 = 5.0;
const FLAT_BASE_CORE: f32 = 0.85;
const FLAT_BASE_REACH: f32 = 0.75;
const LEAN_SHEAR: f32 = 0.5;

/// The streams of one layer's noises: its cloud_layer_seed hashed with a salt per noise, so each noise has its own
/// lattice and a new seed moves them all.
#[derive(Clone, Copy, Debug)]
pub struct Streams(pub [u32; 8]);

impl Streams {
    pub fn of(seed: u32) -> Streams {
        Streams(std::array::from_fn(|k| hash_u32(seed ^ 0x27d4_eb2fu32.wrapping_mul(k as u32 + 1))))
    }
}

/// A layer's wind frame on its plane: a point's (along, across) in its cells, the pattern moved `drift` cells downwind.
#[derive(Clone, Copy, Debug)]
pub struct WindFrame {
    down: Vec2f,
    drift: f32,
}

impl WindFrame {
    pub fn new(down: Vec2f, drift_cells: f32) -> WindFrame {
        WindFrame { down, drift: drift_cells }
    }

    /// (along, across) of the plane point `p` (cells east and south of the viewer).
    pub fn of(&self, p: Vec2f) -> Vec2f {
        vec2f(p.x * self.down.x + p.y * self.down.y - self.drift, p.y * self.down.x - p.x * self.down.y)
    }

    /// Its way downwind (east, south) and its drift (cells along), as the GPU export's uniform takes them (design §12.3).
    pub(crate) fn gpu(&self) -> [f32; 3] {
        [self.down.x, self.down.y, self.drift]
    }

    /// The plane's (east, south) of an offset `v` given as (along, across): `of`'s turn undone, without the drift.
    pub fn to_plane(&self, v: Vec2f) -> Vec2f {
        vec2f(v.x * self.down.x - v.y * self.down.y, v.x * self.down.y + v.y * self.down.x)
    }
}

/// Octave counts and the distance LOD's weight between them (1: all the fine ones).
#[derive(Clone, Copy, Debug)]
pub struct Lod {
    pub fine: u32,
    pub broad: u32,
    pub detail: f32,
}

impl Lod {
    /// A whole number of octaves, no blend (a table's measurement, a plane cover).
    pub fn whole(octaves: u32) -> Lod {
        Lod { fine: octaves, broad: octaves, detail: 1.0 }
    }

    /// At most `most` octaves of either count.
    pub fn capped(self, most: u32) -> Lod {
        Lod { fine: self.fine.min(most), broad: self.broad.min(most), detail: self.detail }
    }
}

/// 1 near the point overhead, 0 far away (clouds.rs `detail_at`).
pub fn detail_at(distance_cells: f32) -> f32 {
    1.0 - smoothstep(DETAIL_FADE_START, DETAIL_FADE_END, distance_cells)
}

/// fbm2 whose fine octaves fade out with `lod.detail` (clouds.rs `fbm_lod`'s rule).
pub fn lod_fbm(p: Vec2f, lod: Lod, seed: u32) -> f32 {
    if lod.detail >= 1.0 || lod.fine == lod.broad {
        return fbm2(p.x, p.y, lod.fine, seed);
    }
    let low = fbm2(p.x, p.y, lod.broad, seed);
    if lod.detail <= 0.0 {
        return low;
    }
    low + (fbm2(p.x, p.y, lod.fine, seed) - low) * lod.detail
}

/// The cover threshold for `lod`: the fine and the broad octave counts' tables, blended by the distance-LOD weight,
/// each measured through `n` (MEASURE_SEED's streams) on first use.
pub fn lod_threshold(remap: &CoverRemap, lod: Lod, n: &dyn Fn(u32, f32, f32) -> f32) -> f32 {
    let fine = remap.threshold(lod.fine, n);
    if lod.detail >= 1.0 || lod.fine == lod.broad {
        return fine;
    }
    let broad = remap.threshold(lod.broad, n);
    broad + (fine - broad) * lod.detail.max(0.0)
}

/// The Stratocumulus' noise: its cells over the broad `field`, in its share.
fn cells_over(f: Vec2f, field: f32, s: &Streams) -> f32 {
    let cells = billow2(f.x * SC_CELL_FREQ + 1.7, f.y * SC_CELL_FREQ + 4.1, 1, s.0[3]);
    SC_CELL_SHARE * cells + (1.0 - SC_CELL_SHARE) * field
}

/// The Cumulus' noise: its clumps over the broad `field`, in their share.
fn clumps_over(f: Vec2f, field: f32, s: &Streams) -> f32 {
    let clumps = billow2(f.x * CU_CLUMP_FREQ + 2.9, f.y * CU_CLUMP_FREQ + 0.6, 1, s.0[3]);
    CU_CLUMP_SHARE * clumps + (1.0 - CU_CLUMP_SHARE) * field
}

/// Where a Cirrus' streams break along their length, at a point `w` of their bent stretched frame: the share of them kept.
fn stream_breaks(w: Vec2f, s: &Streams) -> f32 {
    let n = value2(w.x * BREAK_ALONG + 7.3, w.y * BREAK_ACROSS + 2.9, s.0[4] ^ 0x2c1b_3c6d);
    BREAK_FLOOR + (1.0 - BREAK_FLOOR) * smoothstep(BREAK_FROM, BREAK_TO, n)
}

// --- Cirrus ---

/// The Earth's radius the sheets' plane is met on (volumetric.rs PLANET_M): a ray's local slope at its hit.
const EARTH_M: f32 = 6_360_000.0;

/// A Cirrus' bundles (design §4.3's streaks and hooks), sparse over its faint veil: one a cell of a grid in its
/// streams' bent frame (cells of Size), BUNDLE_EMPTY of the cells holding none, each row of cells shifted along by its
/// own amount so that no two rows line up. A bundle is a line from its head downwind, at most BUNDLE_LENGTH cells long
/// (times the root of Streak length over BUNDLE_STREAK, and 1 + FALL_LENGTH x Fall streaks) and so BUNDLE_SPAN of its
/// cell; its size runs from BUNDLE_LEAST to all, most of them small (the square of a uniform), and it turns off the
/// wind by up to BUNDLE_TILT.
const BUNDLE_STREAK: f32 = 7.0;
const BUNDLE_LENGTH: f32 = 2.0;
const BUNDLE_SPAN: f32 = 0.75;
const BUNDLE_EMPTY: f32 = 0.2;
const BUNDLE_LEAST: f32 = 0.2;
const BUNDLE_TILT: f32 = 0.03;
/// Its width: at its largest BUNDLE_WIDTH cells either side of its line (a smaller bundle by the root of its size),
/// BUNDLE_HEAD of that at its head, fanning out by BUNDLE_FAN of it to its tail's end; a soft bell (1 - x^2)^2 across.
const BUNDLE_WIDTH: f32 = 0.05;
const BUNDLE_HEAD: f32 = 0.5;
const BUNDLE_FAN: f32 = 4.0;
/// The hook (uncinus): over the first BUNDLE_CURL cells from its head the line swings aside by up to BUNDLE_BEND of that
/// at Hooks 1, by its own share and to its own side (a cube, so it curls at the head); its tail sweeps BUNDLE_SWEEP of
/// its length the other way.
const BUNDLE_CURL: f32 = 0.15;
const BUNDLE_BEND: f32 = 1.0;
const BUNDLE_SWEEP: f32 = 0.03;
/// Its strength along it: a dense knot at the head, BUNDLE_KNOT more BUNDLE_KNOT_AT cells from its head (a bell
/// BUNDLE_KNOT_SPAN cells wide), falling to BUNDLE_TAIL at its end, fraying out over its last BUNDLE_FRAY; its head's
/// front is round; its brightness its own, from BUNDLE_DIM to all.
const BUNDLE_KNOT: f32 = 1.2;
const BUNDLE_KNOT_AT: f32 = 0.05;
const BUNDLE_KNOT_SPAN: f32 = 0.06;
const BUNDLE_TAIL: f32 = 0.35;
const BUNDLE_FRAY: f32 = 0.35;
const BUNDLE_DIM: f32 = 0.5;
/// A row of cells is BUNDLE_ROW cells across; a head lies within BUNDLE_JITTER of a row of its row's middle. A bundle
/// reaches across several rows (its widest bell, its hook, its sweep and its tilt), and a point looks in as many rows
/// either side of its own.
const BUNDLE_ROW: f32 = 0.12;
const BUNDLE_JITTER: f32 = 0.5;
/// Where a bundle shows: over the veil's density from 0 to BUNDLE_GATE.
const BUNDLE_GATE: f32 = 0.5;
/// Low down, where a ray runs through the layer's depth over BAND_FROM to BAND_TO cells along the view (elevations of
/// some 22 down to 8 degrees at the defaults), the bundles merge into their mean: a soft band along the horizon.
const BAND_FROM: f32 = 0.08;
const BAND_TO: f32 = 0.25;

/// A bundle's fibres: lanes along it in its fanning frame (across in half widths of its fan there, so they spread from
/// the head; along in cells from its head). A lane is FIBRE_LANE half widths wide (FIBRE_FINE at Fineness 1), a lane
/// cell FIBRE_CELL of the bundle's length long but FIBRE_CELL_LEAST to FIBRE_CELL_MOST cells (a tuft's fibres run its
/// tail, a long streak's lie end to end). The lanes wave across by up to FIBRE_WAVE of a lane over FIBRE_WAVE_CELLS
/// lane cells. Each lane
/// cell starts one fibre, FIBRE_SPAN_LEAST to all of a cell long, rising over its first FIBRE_RISE and fading over its
/// last FIBRE_FALL, a soft line (1 - g^2)^3 FIBRE_BELL of its lane wide either side, of its own brightness from
/// FIBRE_LOW and place across its lane (within FIBRE_JITTER). It is there where the bundle's strength beats its own hash
/// (within FIBRE_SOFT), so the bundle's edges and tail fray into separate fibres.
const FIBRE_LANE: f32 = 0.16;
const FIBRE_FINE: f32 = 0.08;
const FIBRE_CELL: f32 = 0.3;
const FIBRE_CELL_LEAST: f32 = 0.08;
const FIBRE_CELL_MOST: f32 = 0.25;
const FIBRE_WAVE: f32 = 2.0;
const FIBRE_WAVE_CELLS: f32 = 1.5;
const FIBRE_SPAN_LEAST: f32 = 0.6;
const FIBRE_RISE: f32 = 0.2;
const FIBRE_FALL: f32 = 0.4;
const FIBRE_BELL: f32 = 0.8;
const FIBRE_JITTER: f32 = 0.6;
const FIBRE_LOW: f32 = 0.5;
const FIBRE_SOFT: f32 = 0.4;
/// A bundle's smooth share under its fibres is FIBRE_CORE x (1 - Fineness): at Fineness 1 its fibres alone carry it.
const FIBRE_CORE: f32 = 0.6;
/// A box blur of a width is the bell's widening by BLUR_BELL of it.
const BLUR_BELL: f32 = 0.5;

/// The Cirrus look's numbers (design §2.3's ice family on a sheet), as the look rounds tune them. `HDRI_CIRRUS_LOOK`
/// (for example `ca=0,4,0.3,0.6;cb=400,2.5,0.06,4;cc=0.3,60,0,0`, the defaults) overrides them, read once: a look round's
/// knobs, unset in normal runs.
#[derive(Clone, Copy, Debug)]
struct CirrusLook {
    /// ca.x: a flat share of the sun on top of the ice lobes (0: the shared law's ice phase alone).
    spread: f32,
    /// ca.y: the optical depth straight up of a bundle's full strength at Opacity 1; ca.z: the least sine of the ray's
    /// slope through the sheet the slant path reads; ca.w: the share of the slant path taken (alpha = 1 - e^(-tau (1 +
    /// ca.w (1 / mu - 1)))).
    tau: f32,
    mu_min: f32,
    slant: f32,
    /// cb.x: the layer's depth in metres, which a ray crosses over its depth / tan(elevation) along the view and so blurs
    /// the bundles across their lines (low cirrus seen through its depth is a soft band); cb.y: the finest fibre kept,
    /// in footprints of the texel and that blur; cb.z: the veil's optical depth over a bundle's full strength; cb.w:
    /// Opacity's power in the optical depth (4: Opacity 0.5 is a sixteenth of ca.y, a thin cirrus).
    depth_m: f32,
    texels: f32,
    veil: f32,
    opacity_power: f32,
    /// cc.x: the plane's mean optical depth over a bundle's full strength, which shades lower layers; cc.y: a fibre's
    /// own depth in metres, which blurs it as the layer's depth blurs a bundle.
    plane: f32,
    fibre_depth_m: f32,
}

fn cirrus_look() -> &'static CirrusLook {
    static LOOK: std::sync::OnceLock<CirrusLook> = std::sync::OnceLock::new();
    LOOK.get_or_init(|| {
        let mut k = [[0.0, 4.0, 0.3, 0.6], [400.0, 2.5, 0.06, 4.0], [0.3, 60.0, 0.0, 0.0]];
        let spec = std::env::var("HDRI_CIRRUS_LOOK").unwrap_or_default();
        for part in spec.split(';') {
            let Some((name, values)) = part.split_once('=') else { continue };
            let v: Vec<f32> = values.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            let slot = match name.trim() {
                "ca" => 0,
                "cb" => 1,
                "cc" => 2,
                _ => continue,
            };
            if v.len() == 4 {
                k[slot] = [v[0], v[1], v[2], v[3]];
            }
        }
        if !spec.is_empty() {
            log!("hdri clouds: cirrus look knobs {spec}");
        }
        CirrusLook {
            spread: k[0][0],
            tau: k[0][1],
            mu_min: k[0][2],
            slant: k[0][3],
            depth_m: k[1][0],
            texels: k[1][1],
            veil: k[1][2],
            opacity_power: k[1][3],
            plane: k[2][0],
            fibre_depth_m: k[2][1],
        }
    })
}

/// A Cirrus' optical depth straight up at a bundle's full strength for its Opacity (ca.y x Opacity^cb.w).
fn cirrus_tau(opacity: f32) -> f32 {
    let look = cirrus_look();
    look.tau * opacity.max(0.0).powf(look.opacity_power)
}

/// The Cirrostratus (design §4.3; lane hi round 7): a thin veil of ice. Its plane is the remap's veil over broad patches
/// (CS_PATCH_FREQ cycles a cell: some 16 km at the default Size, so Cover 0.9 is a veil over the sky with a few clear
/// gaps), its optical depth straight up `tau` (CS_TAU_THIN + CS_TAU_PER_MILK x Milkiness^2: 0.2 at the default 0.45,
/// seen 1 / mu as deep along a slant, so it whitens toward the horizon), thickened and thinned by its mottles (Mottling,
/// fbm at a cell) and its fibres (Fibres, ridged noise CS_FIBRE_STRETCH times longer along the wind than across). Its
/// light is single scattering through ice: a forward lobe (the glow round the sun) over a broad one, and the 22-degree
/// halo, red on its sharp inner edge and fading outward, all with the sun's light; the dome's light on it besides.
const CS_OCTAVES: u32 = 3;
const CS_BROAD_OCTAVES: u32 = 2;
const CS_PATCH_FREQ: f32 = 0.125;
/// Its edge in the remap: a veil thins out over a wide band.
const CS_EDGE: f32 = 0.35;
const CS_MOTTLE_OCTAVES: u32 = 3;
const CS_FIBRE_FREQ: f32 = 3.0;
const CS_FIBRE_OCTAVES: u32 = 3;
/// ridged2's mean, round which the fibres move the depth.
const CS_RIDGED_MEAN: f32 = 0.4;
/// The halo's inner edge per colour (degrees from the sun: red bends least) and how sharp it is.
const CS_HALO_EDGE_DEG: [f32; 3] = [21.7, 21.85, 22.0];
const CS_HALO_SHARP_DEG: f32 = 0.6;

/// The Cirrostratus look's numbers, as the look rounds tune them. `HDRI_CIRROSTRATUS_LOOK` (for example
/// `sa=0.05,0.75,0.1,0.6;sb=1,0.75,0.3,0.6;sc=2.5,1.6,1.2,12`, the defaults) overrides them, read once: a look round's
/// knobs, unset in normal runs.
#[derive(Clone, Copy, Debug)]
struct CirrostratusLook {
    /// sa: its depth at Milkiness 0, its depth per Milkiness^2, the least sine of the ray's slope its slant reads, the
    /// forward lobe's share of the phase (at Silver lining 1).
    tau_thin: f32,
    tau_per_milk: f32,
    mu_min: f32,
    forward: f32,
    /// sb: the phase's gain, the forward and the broad lobe's g, the halo's peak (per steradian, at Halo 1). Fitted to
    /// 096_1228's glow (its median luminance in rings round the sun within 12 % of the real sky's from 5 to 60 degrees;
    /// at g 0.85 the ring 5-10 degrees off the sun was 1.8 times the real one's).
    gain: f32,
    g_forward: f32,
    g_broad: f32,
    halo_peak: f32,
    /// sc: how fast the halo fades outward (degrees), how far Mottling moves the depth (e^(+-)), how far Fibres move it
    /// (a share), the fibres' stretch along the wind.
    halo_fall_deg: f32,
    mottle_depth: f32,
    fibre_depth: f32,
    fibre_stretch: f32,
}

fn cirrostratus_look() -> &'static CirrostratusLook {
    static LOOK: std::sync::OnceLock<CirrostratusLook> = std::sync::OnceLock::new();
    LOOK.get_or_init(|| {
        let mut k = [[0.05, 0.75, 0.1, 0.6], [1.0, 0.75, 0.3, 0.6], [2.5, 1.6, 1.2, 12.0]];
        let spec = std::env::var("HDRI_CIRROSTRATUS_LOOK").unwrap_or_default();
        for part in spec.split(';') {
            let Some((name, values)) = part.split_once('=') else { continue };
            let v: Vec<f32> = values.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            let slot = match name.trim() {
                "sa" => 0,
                "sb" => 1,
                "sc" => 2,
                _ => continue,
            };
            if v.len() == 4 {
                k[slot] = [v[0], v[1], v[2], v[3]];
            }
        }
        if !spec.is_empty() {
            log!("hdri clouds: cirrostratus look knobs {spec}");
        }
        CirrostratusLook {
            tau_thin: k[0][0],
            tau_per_milk: k[0][1],
            mu_min: k[0][2],
            forward: k[0][3],
            gain: k[1][0],
            g_forward: k[1][1],
            g_broad: k[1][2],
            halo_peak: k[1][3],
            halo_fall_deg: k[2][0],
            mottle_depth: k[2][1],
            fibre_depth: k[2][2],
            fibre_stretch: k[2][3],
        }
    })
}

/// The Cirrostratus' phase per steradian at the cosine `cos_sun` from the sun, per colour: its ice lobes (Silver lining
/// scaling the forward one's share) and Halo's ring.
fn cirrostratus_phase(cos_sun: f32, lining: f32, halo: f32) -> Vec3f {
    let look = cirrostratus_look();
    let forward = look.forward * lining.clamp(0.0, 1.0);
    let lobes = look.gain * ((1.0 - forward) * crate::clouds::hg_phase(cos_sun, look.g_broad) + forward * crate::clouds::hg_phase(cos_sun, look.g_forward));
    if !(halo > 0.0) {
        return vec3f(lobes, lobes, lobes);
    }
    let deg = cos_sun.clamp(-1.0, 1.0).acos().to_degrees();
    let ring = |edge: f32| {
        let past = deg - edge;
        smoothstep(-CS_HALO_SHARP_DEG, 0.0, past) * (-past.max(0.0) / look.halo_fall_deg.max(0.01)).exp()
    };
    let k = look.halo_peak * halo;
    vec3f(lobes + k * ring(CS_HALO_EDGE_DEG[0]), lobes + k * ring(CS_HALO_EDGE_DEG[1]), lobes + k * ring(CS_HALO_EDGE_DEG[2]))
}

/// A Cirrus' streams' frame at `f` (wind frame, cells): Streak length stretches it along the wind, Fall streaks slide
/// strands downwind by different amounts, and the older veil's warp bends it, so the streams curve and are not
/// parallel. Its point `w`: x along the streams, y across.
fn cirrus_frame(f: Vec2f, streak: f32, fall: f32, s: &Streams) -> Vec2f {
    let root = streak.max(1.0).sqrt();
    let mut q = vec2f(f.x * STREAK_SCALE / root, f.y * STREAK_SCALE * root);
    if fall > 0.0 {
        q.x += fall * FALL_SHEAR * (value2(q.y * 0.6, 0.5, s.0[4]) - 0.5);
    }
    let wx = fbm2(q.x * 0.5, q.y * 0.5, 2, s.0[5]) - 0.5;
    let wy = fbm2(q.x * 0.5 + 5.2, q.y * 0.5 + 1.3, 2, s.0[5]) - 0.5;
    vec2f(q.x + 1.5 * wx, q.y + 1.5 * wy)
}

/// A Cirrus' bundles at its rows (cells of its streams' bent frame, unstretched).
#[derive(Clone, Copy, Debug)]
struct Bundles {
    /// The longest bundle and the widest half width, and the hook's swing at Hooks (a share of a bundle's length).
    length: f32,
    width: f32,
    bend: f32,
    /// A cell along and a row across, the furthest a bundle's bell reaches across its line, and how many rows either
    /// side of a point's own can hold a bundle that reaches it.
    cell: f32,
    row: f32,
    reach: f32,
    rows: i32,
}

/// The strongest bundle at a point: its strength, its length (cells), where the point lies on it (`across`: half widths
/// of its fan there; `from_head`: cells along it from its head), its own number (its fibres' seed) and its fan's half
/// width there (cells).
#[derive(Clone, Copy, Debug, Default)]
struct BundleAt {
    strength: f32,
    length: f32,
    across: f32,
    id: u32,
    half_width: f32,
    /// How far the point lies from the head along the bundle (cells).
    from_head: f32,
}

impl Bundles {
    fn new(streak: f32, hooks: f32, fall: f32) -> Bundles {
        let root = (streak.max(1.0) / BUNDLE_STREAK).sqrt();
        let length = BUNDLE_LENGTH * root * (1.0 + FALL_LENGTH * fall);
        let width = BUNDLE_WIDTH / root;
        let bend = BUNDLE_BEND * hooks;
        // Across its line a bundle reaches its tail's widest bell, its hook's swing, its sweep and its tilt.
        let reach = width * (BUNDLE_HEAD + BUNDLE_FAN) + bend * BUNDLE_CURL + (BUNDLE_SWEEP + BUNDLE_TILT) * length;
        let rows = ((reach + 0.5 * BUNDLE_JITTER * BUNDLE_ROW) / BUNDLE_ROW).ceil() as i32;
        Bundles { length, width, bend, cell: length / BUNDLE_SPAN, row: BUNDLE_ROW, reach, rows }
    }

    /// The strongest bundle at `g` (cells of the streams' bent frame), its bell widened by `blur` (cells across its line:
    /// the texel's footprint and the layer's depth seen along the view) and its strength lowered as much as it widens.
    fn at(&self, g: Vec2f, blur: f32, seed: u32) -> BundleAt {
        let byte = |h: u32, k: u32| ((h >> (8 * k)) & 0xff) as f32 * (1.0 / 255.0);
        let mut best = BundleAt::default();
        let iy = (g.y / self.row).floor() as i32;
        for row in iy - self.rows..=iy + self.rows {
            // A row whose bundles' lines all lie further off than their reach holds none that reaches `g`.
            if (g.y - (row as f32 + 0.5) * self.row).abs() > self.reach + 0.5 * BUNDLE_JITTER * self.row {
                continue;
            }
            let row_hash = hash_u32(seed ^ (row as u32).wrapping_mul(0x9e37_79b9));
            let shift = (row_hash & 0xffff) as f32 * (1.0 / 65536.0);
            let column = (g.x / self.cell - shift).floor();
            let h = hash_u32(row_hash ^ (column as i32 as u32).wrapping_mul(0x85eb_ca6b));
            let more = hash_u32(h ^ 0x5bd1_e995);
            if byte(more, 0) < BUNDLE_EMPTY {
                continue;
            }
            let size = BUNDLE_LEAST + (1.0 - BUNDLE_LEAST) * byte(more, 1) * byte(more, 1);
            let length = self.length * size;
            let width = self.width * size.sqrt();
            let head_along = (column + shift + byte(h, 0) * (1.0 - size * BUNDLE_SPAN)) * self.cell;
            let head_across = (row as f32 + 0.5 + BUNDLE_JITTER * (byte(h, 1) - 0.5)) * self.row;
            let tilt = BUNDLE_TILT * (2.0 * byte(h, 2) - 1.0);
            let (rx, ry) = (g.x - head_along, g.y - head_across);
            let along = rx + tilt * ry;
            let front = width * BUNDLE_HEAD;
            if !(along > -front && along < length) {
                continue;
            }
            let t = along / length;
            let s = t.clamp(0.0, 1.0);
            // The hook swings the line aside at the head, to the bundle's own side; the tail sweeps back.
            let side = if byte(h, 3) < 0.5 { -1.0 } else { 1.0 };
            let curl = (1.0 - along.max(0.0) / BUNDLE_CURL).max(0.0);
            let hook = self.bend * BUNDLE_CURL * byte(more, 2) * curl * curl * curl;
            let off = ry - tilt * rx - side * (hook - BUNDLE_SWEEP * length * s * s);
            let half = width * (BUNDLE_HEAD + BUNDLE_FAN * s);
            let wide = (half * half + blur * blur).sqrt().min(self.reach);
            let x = off / wide;
            if !(x.abs() < 1.0) {
                continue;
            }
            let bell = (1.0 - x * x) * (1.0 - x * x);
            let round = if along < 0.0 { 1.0 - (along / front) * (along / front) } else { 1.0 };
            let k = (along - BUNDLE_KNOT_AT) / BUNDLE_KNOT_SPAN;
            let knot = 1.0 + BUNDLE_KNOT * (-k * k).exp();
            let tail = BUNDLE_TAIL + (1.0 - BUNDLE_TAIL) * (1.0 - s) * (1.0 - s);
            let fade = round * tail * knot * smoothstep(1.0, 1.0 - BUNDLE_FRAY, t);
            let strength = bell * fade * (half / wide) * (BUNDLE_DIM + (1.0 - BUNDLE_DIM) * byte(more, 3));
            if strength > best.strength {
                best = BundleAt { strength, length, across: off / half, id: h, half_width: half, from_head: along };
            }
        }
        best
    }
}

impl Bundles {
    /// Its bundles' mean strength over the plane (64 x 64 points of a 32 x 32 cell square at a fixed jitter): what
    /// they merge into where a ray crosses many of them one behind the other. Once per sheet.
    fn mean(&self, seed: u32) -> f32 {
        let jitter = hash_u32(0x3c6e_f372);
        let mut sum = 0.0f32;
        for j in 0..64 {
            for i in 0..64 {
                let x = (i as f32 + crate::noise::hash01(i, j, 0, jitter)) * 0.5;
                let y = (j as f32 + crate::noise::hash01(i, j, 1, jitter)) * 0.5;
                sum += self.at(vec2f(x, y), 0.0, seed).strength;
            }
        }
        sum / 4096.0
    }
}

/// One octave of a bundle's fibres at (`along`, `across`) in its lane cells and lanes, for the bundle's `strength`: the
/// brightest fibre there.
fn fibre_lane(along: f32, across: f32, strength: f32, seed: u32) -> f32 {
    let (i, j) = (along.floor() as i32, across.floor() as i32);
    let mut most = 0.0f32;
    for dj in -1..=1 {
        let lane = hash_u32(seed ^ (j + dj) as u32);
        for di in -1..=0 {
            let h = hash_u32(lane ^ ((i + di) as u32).wrapping_mul(0x9e37_79b9));
            let byte = |k: u32| ((h >> (8 * k)) & 0xff) as f32 * (1.0 / 255.0);
            let threshold = byte(3);
            let present = smoothstep(threshold - FIBRE_SOFT, threshold + FIBRE_SOFT, strength);
            if !(present > 0.0) {
                continue;
            }
            let start = (i + di) as f32 + byte(0);
            let t = (along - start) / (FIBRE_SPAN_LEAST + (1.0 - FIBRE_SPAN_LEAST) * byte(1));
            if !(t > 0.0 && t < 1.0) {
                continue;
            }
            let g = (across - ((j + dj) as f32 + 0.5 + FIBRE_JITTER * (byte(2) - 0.5))) / FIBRE_BELL;
            if !(g.abs() < 1.0) {
                continue;
            }
            let q = 1.0 - g * g;
            let bright = FIBRE_LOW + (1.0 - FIBRE_LOW) * ((hash_u32(h) & 0xff) as f32 * (1.0 / 255.0));
            let taper = smoothstep(0.0, FIBRE_RISE, t) * smoothstep(1.0, 1.0 - FIBRE_FALL, t);
            most = most.max(bright * q * q * q * taper * present);
        }
    }
    most
}

/// One lane octave's mean where every fibre is there (64 x 64 points at a fixed jitter, measured once): what an octave
/// finer than its blur stands for, so the fibres keep their mean at every size and distance.
fn fibre_mean() -> f32 {
    static MEAN: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *MEAN.get_or_init(|| {
        let jitter = hash_u32(0x3c6e_f372);
        let mut sum = 0.0f32;
        for j in 0..64 {
            for i in 0..64 {
                let x = (i as f32 + crate::noise::hash01(i, j, 0, jitter)) * 0.37;
                let y = j as f32 + crate::noise::hash01(i, j, 1, jitter);
                sum += fibre_lane(x, y, 2.0, 0x51ed_270b);
            }
        }
        sum / 4096.0
    })
}

/// A bundle's fibres at `at` (its frame), one octave of lanes `lane` half widths wide, `shown` as far as the texel and
/// a fibre's depth show it (fractional: finer than that it fades toward its mean): their value over their mean where
/// they all are, so a bundle's opacity is its strength on the whole and its fibres its texture; where few are there, each
/// keeps its brightness, and the bundle's edges and tail fray.
fn bundle_fibres(at: &BundleAt, shown: f32, lane: f32, seed: u32) -> f32 {
    let show = shown.clamp(0.0, 1.0);
    if !(show > 0.0) {
        return 1.0;
    }
    let mean = fibre_mean() * at.strength.clamp(1.0e-3, 1.0);
    let along = at.from_head / (FIBRE_CELL * at.length).clamp(FIBRE_CELL_LEAST, FIBRE_CELL_MOST);
    // The lanes wave across, slowly along them: where the point lies across moves, no ridge is drawn.
    let wave = FIBRE_WAVE * (value2(along / FIBRE_WAVE_CELLS, at.across / lane * 0.25, seed ^ 0x6a09_e667) - 0.5);
    let f = fibre_lane(along, at.across / lane + wave, at.strength, seed);
    (mean + (f - mean) * show) / mean
}

/// A smooth wave in [0, 1] of period 1, 1 at the half periods: a smoothstepped triangle, polynomial and hash-free.
pub fn wave(x: f32) -> f32 {
    let f = x - x.floor();
    let tri = 1.0 - (2.0 * f - 1.0).abs();
    tri * tri * (3.0 - 2.0 * tri)
}

/// Where a view ray of sin(elevation) d.y > 0 meets a plane `height_m` up on the curved Earth: metres east and south.
pub fn plane_hit(d: Vec3f, height_m: f32) -> Vec2f {
    let t = shell_distance(d.y, height_m);
    vec2f(d.x * t, d.z * t)
}

/// The horizon fade of an alpha: smoothstep(0, HORIZON_FADE, sin el).
pub fn horizon_fade(sin_el: f32) -> f32 {
    smoothstep(0.0, HORIZON_FADE, sin_el)
}

/// Local copy (as in clouds.rs), so no glob can make the name ambiguous.
fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A sheet type's recipe and the rows it reads.
#[derive(Clone, Copy, Debug)]
pub enum SheetRecipe {
    /// `bundle_mean`: its bundles' mean strength over the plane (`Bundles::mean`, measured at build); `tau`: its
    /// optical depth at a bundle's full strength (`cirrus_tau` of its Opacity).
    Cirrus { streak: f32, fineness: f32, hooks: f32, fall: f32, patch: f32, opacity: f32, bundle_mean: f32, tau: f32 },
    Stratocumulus { shading: f32, billows: f32, rolls: f32 },
    Cumulus { puffiness: f32, flat_base: f32, towers: f32, clustering: f32, streets: f32, lean: f32 },
    /// `tau`: its optical depth straight up where its veil is whole (before its mottles and fibres).
    Cirrostratus { tau: f32, mottling: f32, fibres: f32, halo: f32 },
}

impl SheetRecipe {
    /// The fine and the broad octave counts.
    fn octaves(&self) -> (u32, u32) {
        match self {
            SheetRecipe::Cirrus { .. } => (CIRRUS_OCTAVES, CIRRUS_BROAD_OCTAVES),
            SheetRecipe::Stratocumulus { .. } => (SC_OCTAVES, DECK_BROAD_OCTAVES),
            SheetRecipe::Cumulus { .. } => (DECK_OCTAVES, DECK_BROAD_OCTAVES),
            SheetRecipe::Cirrostratus { .. } => (CS_OCTAVES, CS_BROAD_OCTAVES),
        }
    }

    /// The distance LOD's weight at `detail` (detail_at's): the noise's fine octaves and the threshold's blend read it,
    /// so the cover stays the share of the sky toward the horizon. A Cirrus' noise has no fine octaves: its detail is
    /// its bundles', which follow the texel and the layer's depth.
    fn lod_weight(&self, detail: f32) -> f32 {
        detail
    }

    /// The fields that change the noise's spread, in declaration order: the fingerprint's (design §3.1).
    fn shape_fields(&self) -> Vec<f32> {
        match *self {
            SheetRecipe::Cirrus { streak, fineness, hooks, fall, patch, .. } => vec![streak, fineness, hooks, fall, patch],
            SheetRecipe::Stratocumulus { billows, rolls, .. } => vec![billows, rolls],
            SheetRecipe::Cumulus { puffiness, clustering, streets, .. } => vec![puffiness, clustering, streets],
            SheetRecipe::Cirrostratus { .. } => Vec::new(),
        }
    }

    /// What it hides at a density: the Cirrus' Opacity share; the deck's Beer-Lambert (clouds.rs `deck_opacity`); the
    /// Cumulus' the same, Towers deepening it, normalised so density 1 is opaque.
    fn opacity(&self, density: f32) -> f32 {
        match *self {
            // Its veil's and its bundles' mean optical depth straight up (the shade it casts on a lower layer).
            SheetRecipe::Cirrus { tau, .. } => 1.0 - (-tau * cirrus_look().plane * density).exp(),
            SheetRecipe::Stratocumulus { .. } => deck_opacity(density),
            SheetRecipe::Cumulus { towers, .. } => {
                let k = CUMULUS_THICKNESS * (1.0 + 2.0 * towers);
                (1.0 - (-k * density).exp()) / (1.0 - (-k).exp())
            }
            SheetRecipe::Cirrostratus { tau, .. } => 1.0 - (-tau * density).exp(),
        }
    }

    /// A Cirrus' noise n at a point `f` of the wind frame (cells) and its streams' frame point there: its streams, where
    /// its faint veil lies and its bundles gather, broken along their length, gathered over tens of km and opened by
    /// Patchiness.
    fn cirrus_field(&self, f: Vec2f, s: &Streams) -> (f32, Vec2f) {
        let SheetRecipe::Cirrus { streak, fall, patch, .. } = *self else {
            return (0.0, Vec2f::default());
        };
        let w = cirrus_frame(f, streak, fall, s);
        let field = fbm2(w.x * TUFT_ALONG, w.y * TUFT_ACROSS, CIRRUS_BROAD_OCTAVES, s.0[0]);
        let stream = smoothstep(TUFT_FROM, TUFT_TO, field) * stream_breaks(w, s);
        // Where the cirrus gathers, over tens of km: its streams, then open blue.
        let gather = smoothstep(GATHER_FROM, GATHER_TO, value2(f.x * GATHER_FREQ + 3.7, f.y * GATHER_FREQ + 9.1, s.0[6]));
        // Patchiness: a broad mask opens holes in the field.
        let slow = if patch > 0.0 { 1.0 - patch + patch * smoothstep(0.3, 0.7, fbm2(f.x * 0.18, f.y * 0.18, 2, s.0[2])) } else { 1.0 };
        ((stream * (GATHER_FLOOR + (1.0 - GATHER_FLOOR) * gather) * slow).clamp(0.0, 1.0), w)
    }

    /// The noise n in [0, 1] at a point `f` of the wind frame (cells), and with it a heap's slow factor (the masks and
    /// rows) that the self-shadow's steps reuse (a Cirrus' 1).
    fn noise(&self, f: Vec2f, lod: Lod, s: &Streams) -> (f32, f32) {
        match *self {
            SheetRecipe::Cirrus { .. } => (self.cirrus_field(f, s).0, 1.0),
            SheetRecipe::Stratocumulus { billows, rolls, .. } => {
                // Cells: one billowed octave's heaps, parted along its creases, over the broad field that gathers them
                // into patches.
                let mut n = cells_over(f, lod_fbm(f, lod, s.0[0]), s);
                // Billows: lumps on the cells, at a third of a cell.
                if billows > 0.0 {
                    n += billows * LUMP_AMOUNT * (billow2(f.x * LUMP_FREQ + 3.1, f.y * LUMP_FREQ + 7.7, 2, s.0[1]) - BILLOW_MEAN);
                }
                // Rolls: a warped wave across the wind, two cells (twice Size) from roll to roll.
                let slow = if rolls > 0.0 {
                    let warp = value2(f.x * 0.25, f.y * 0.25, s.0[2]) - 0.5;
                    1.0 - rolls * ROLL_DEPTH * (1.0 - wave(f.y * 0.5 + 0.35 * warp))
                } else {
                    1.0
                };
                ((n * slow).clamp(0.0, 1.0), slow)
            }
            SheetRecipe::Cumulus { puffiness, clustering, streets, .. } => {
                // Clumps: one billowed octave's heaps, parted along its creases, so the heaps stand apart.
                let mut n = clumps_over(f, lod_fbm(f, lod, s.0[0]), s);
                // Puffiness: billowed lumps on the heaps at two scales, a cauliflower edge.
                if puffiness > 0.0 {
                    n += puffiness * PUFF_AMOUNT * (billow2(f.x * PUFF_FREQ + 1.9, f.y * PUFF_FREQ + 4.3, 2, s.0[1]) - BILLOW_MEAN);
                }
                let mut slow = 1.0;
                // Clustering: a broad mask gathers the heaps into groups.
                if clustering > 0.0 {
                    slow *= 1.0 - clustering + clustering * smoothstep(0.35, 0.65, fbm2(f.x * 0.2, f.y * 0.2, 2, s.0[2]));
                }
                // Streets: rows along the wind, three cells (three times Size) apart.
                if streets > 0.0 {
                    slow *= 1.0 - streets * STREET_DEPTH * (1.0 - wave(f.y / 3.0));
                }
                ((n * slow).clamp(0.0, 1.0), slow)
            }
            // The veil's broad patches.
            SheetRecipe::Cirrostratus { .. } => (lod_fbm(f * CS_PATCH_FREQ, lod, s.0[0]), 1.0),
        }
    }

    /// The noise a self-shadow step reads, before the slow factor: SHADOW_OCTAVES of the body, a Stratocumulus' an
    /// octave fewer under its cells (the cells carry its shape at that scale).
    fn step_noise(&self, f: Vec2f, s: &Streams) -> f32 {
        match self {
            SheetRecipe::Stratocumulus { .. } => cells_over(f, fbm2(f.x, f.y, SHADOW_OCTAVES - 1, s.0[0]), s),
            SheetRecipe::Cumulus { .. } => clumps_over(f, fbm2(f.x, f.y, SHADOW_OCTAVES - 1, s.0[0]), s),
            SheetRecipe::Cirrus { .. } => fbm2(f.x, f.y, SHADOW_OCTAVES, s.0[0]),
            SheetRecipe::Cirrostratus { .. } => fbm2(f.x * CS_PATCH_FREQ, f.y * CS_PATCH_FREQ, CS_OCTAVES, s.0[0]),
        }
    }

    /// The noise a plane cover reads (the shade on a lower layer, T_above): `octaves` of the body, the rows, and the
    /// Cirrus' stretch and the warp's broadest octave, so the shade follows the strands that cast it.
    fn plane_noise(&self, f: Vec2f, octaves: u32, s: &Streams) -> f32 {
        match *self {
            SheetRecipe::Cirrus { streak, .. } => {
                let root = streak.max(1.0).sqrt();
                let q = vec2f(f.x * STREAK_SCALE / root, f.y * STREAK_SCALE * root);
                let wx = value2(q.x * 0.5, q.y * 0.5, s.0[5]) - 0.5;
                let wy = value2(q.x * 0.5 + 5.2, q.y * 0.5 + 1.3, s.0[5]) - 0.5;
                let w = vec2f(q.x + 1.5 * wx, q.y + 1.5 * wy);
                let stream = smoothstep(TUFT_FROM, TUFT_TO, fbm2(w.x * TUFT_ALONG, w.y * TUFT_ACROSS, octaves, s.0[0])) * stream_breaks(w, s);
                let gather = smoothstep(GATHER_FROM, GATHER_TO, value2(f.x * GATHER_FREQ + 3.7, f.y * GATHER_FREQ + 9.1, s.0[6]));
                stream * (GATHER_FLOOR + (1.0 - GATHER_FLOOR) * gather)
            }
            SheetRecipe::Stratocumulus { rolls, .. } => {
                let n = cells_over(f, fbm2(f.x, f.y, octaves, s.0[0]), s);
                if rolls > 0.0 { n * (1.0 - rolls * ROLL_DEPTH * (1.0 - wave(f.y * 0.5))) } else { n }
            }
            SheetRecipe::Cumulus { streets, .. } => {
                let n = clumps_over(f, fbm2(f.x, f.y, octaves, s.0[0]), s);
                if streets > 0.0 { n * (1.0 - streets * STREET_DEPTH * (1.0 - wave(f.y / 3.0))) } else { n }
            }
            SheetRecipe::Cirrostratus { .. } => fbm2(f.x * CS_PATCH_FREQ, f.y * CS_PATCH_FREQ, octaves, s.0[0]),
        }
    }
}

/// A modern sheet, ready to draw.
pub struct Sheet {
    recipe: SheetRecipe,
    remap: CoverRemap,
    height_m: f32,
    cell_m: f32,
    frame: WindFrame,
    streams: Streams,
    /// MEASURE_SEED's streams, which the tables are measured with.
    measure: Streams,
    /// One released self-shadow step toward the sun, in cells (a Cumulus' sheared by Lean).
    shadow_step: Vec2f,
}

impl Sheet {
    /// The modern `layer` of `genus` at the effective cover `c` (master x shown_cover), or None for a type that is not
    /// a sheet. `downwind` is its wind's way, `height_m` where it stands (`stand_height_m`).
    pub fn build(layer: &CloudLayerParams, genus: CloudGenus, c: f32, map_seed: u32, downwind: Vec2f, sun_dir: Vec3f, height_m: f32) -> Option<Sheet> {
        let edge = |softness: f32| 0.02 + 0.38 * softness;
        let (recipe, size_km, w) = match genus {
            CloudGenus::Cirrus => {
                let b = layer.cirrus_params();
                let bundle_mean = Bundles::new(b.streak_length, b.hooks, b.fall_streaks).mean(cloud_layer_seed(map_seed, layer.seed, genus));
                (SheetRecipe::Cirrus { streak: b.streak_length, fineness: b.fineness, hooks: b.hooks, fall: b.fall_streaks, patch: b.patchiness, opacity: b.opacity, bundle_mean, tau: cirrus_tau(b.opacity) }, b.size_km, CIRRUS_EDGE)
            }
            CloudGenus::Stratocumulus => {
                let b = layer.stratocumulus_params();
                (SheetRecipe::Stratocumulus { shading: b.shading, billows: b.billows, rolls: b.rolls }, b.size_km, edge(b.softness * SC_SOFTNESS))
            }
            CloudGenus::Cumulus => {
                let b = layer.cumulus_params();
                (SheetRecipe::Cumulus { puffiness: b.puffiness, flat_base: b.flat_base, towers: b.towers, clustering: b.clustering, streets: b.streets, lean: b.lean }, b.size_km, edge(b.softness))
            }
            CloudGenus::Cirrostratus => {
                let b = layer.cirrostratus_params();
                let look = cirrostratus_look();
                let tau = look.tau_thin + look.tau_per_milk * b.milkiness * b.milkiness;
                (SheetRecipe::Cirrostratus { tau, mottling: b.mottling, fibres: b.fibres, halo: b.halo }, b.size_km, CS_EDGE)
            }
            _ => return None,
        };
        let cell_m = size_km * 1000.0;
        let stream = cloud_layer_seed(map_seed, layer.seed, genus);
        let drift_cells = layer.drift_km.unwrap_or(0.0) * 1000.0 / cell_m;
        let remap = CoverRemap::new(quantile_set(fingerprint(genus, 0, &recipe.shape_fields())), c, w, stream);
        let sun = sun_dir.normalize();
        let sun = if sun.is_finite() && sun.length() > 0.5 { sun } else { vec3f(0.0, 1.0, 0.0) };
        // The self-shadow's step (clouds.rs's): a deck thickness x tan(sun zenith) sideways, nothing overhead.
        let horizontal = (sun.x * sun.x + sun.z * sun.z).sqrt();
        let mut shadow_step = if horizontal > 1.0e-6 {
            let tan_zenith = (horizontal / sun.y.max(1.0e-3)).min(SHADOW_MAX_TAN);
            let per_step = SHADOW_REACH * tan_zenith / SHADOW_STEPS as f32;
            vec2f(sun.x / horizontal * per_step, sun.z / horizontal * per_step)
        } else {
            Vec2f::default()
        };
        if let SheetRecipe::Cumulus { lean, .. } = recipe {
            shadow_step = shadow_step + downwind * (lean * LEAN_SHEAR);
        }
        Some(Sheet { recipe, remap, height_m, cell_m, frame: WindFrame::new(downwind, drift_cells), streams: Streams::of(stream), measure: Streams::of(MEASURE_SEED), shadow_step })
    }

    /// A Cirrus' cover along `d`, where it meets the plane `t` metres off at the slope `mu`, at `w` of its streams'
    /// frame with its veil's `density` there, for a lookup of a bake whose texels are `texel` radians wide (0: one exact
    /// direction): its optical depth (design §2.3's thin ice), its faint veil and its bundles' fibres, seen along the
    /// slant path through the sheet. The bundles blur across their lines by the texel's footprint there and by the
    /// layer's depth seen along the view, so low cirrus is a soft band; their fibres go as fine as that blur shows.
    fn cirrus_cover(&self, d: Vec3f, t: f32, mu: f32, w: Vec2f, density: f32, texel: f32) -> f32 {
        let SheetRecipe::Cirrus { streak, fineness, hooks, fall, bundle_mean, tau, .. } = self.recipe else {
            return 0.0;
        };
        let look = cirrus_look();
        let mu = mu.max(1.0e-3);
        // How much of the bundles' cross direction (the wind's) and of their own lies along the view.
        let level = (d.x * d.x + d.z * d.z).sqrt();
        let (c, c_down) = if level > 1.0e-6 {
            ((d.z * self.frame.down.x - d.x * self.frame.down.y) / level, (d.x * self.frame.down.x + d.z * self.frame.down.y) / level)
        } else {
            (0.0, 0.0)
        };
        // The texel's footprint across the bundles and along them (stretched by 1 / mu along the view), and how far a ray
        // runs along the view per metre of depth it crosses.
        let footprint = t * texel * (c * c / (mu * mu) + 1.0 - c * c).sqrt();
        let footprint_along = t * texel * (c_down * c_down / (mu * mu) + 1.0 - c_down * c_down).sqrt();
        let run = (1.0 - mu * mu).max(0.0).sqrt() / mu;
        // The layer's depth seen along the view blurs the bundles across their lines.
        let smear = look.depth_m * run * c.abs();
        let blur = (footprint * footprint + smear * smear).sqrt();
        let root = streak.max(1.0).sqrt();
        let g = vec2f(w.x * root / STREAK_SCALE, w.y / (STREAK_SCALE * root));
        let at = Bundles::new(streak, hooks, fall).at(g, BLUR_BELL * blur / self.cell_m, self.streams.0[3]);
        let mut bundle = 0.0;
        if at.strength > 0.0 {
            let lane = FIBRE_LANE + (FIBRE_FINE - FIBRE_LANE) * fineness;
            // The fibres show as far as their lanes are wider than the texel's footprint and a fibre's own depth seen
            // along the view, and their cells longer than the footprint along them and the layer's depth seen along the
            // view: low down, where a ray crosses many fibres one behind the other, they merge into the bundle.
            let lane_m = lane * at.half_width * self.cell_m;
            let fibre_smear = look.fibre_depth_m * run * c.abs();
            let across = (footprint * footprint + fibre_smear * fibre_smear).sqrt();
            let cell_m = (FIBRE_CELL * at.length).clamp(FIBRE_CELL_LEAST, FIBRE_CELL_MOST) * self.cell_m;
            let along_smear = look.depth_m * run * c_down.abs();
            let along = (footprint_along * footprint_along + along_smear * along_smear).sqrt();
            let show = |size: f32, blur: f32| if blur > 0.0 { 1.0 + (size / (look.texels * blur)).max(1.0e-6).log2() } else { 1.0 };
            let shown = show(lane_m, across).min(show(cell_m, along));
            let core = FIBRE_CORE * (1.0 - fineness);
            let fibres = bundle_fibres(&at, shown, lane, at.id ^ self.streams.0[1]);
            bundle = at.strength * (core + (1.0 - core) * fibres) * smoothstep(0.0, BUNDLE_GATE, density);
        }
        // Low down a ray runs through the layer's depth over BAND_FROM to BAND_TO cells: the bundles one behind the
        // other merge into their mean, a soft band (design §4.3).
        let band = smoothstep(BAND_FROM, BAND_TO, look.depth_m * run / self.cell_m);
        let bundle = bundle + (bundle_mean * smoothstep(0.0, BUNDLE_GATE, density) - bundle) * band;
        let tau = tau * (look.veil * density + bundle);
        let slant = 1.0 + (1.0 / mu.max(look.mu_min) - 1.0) * look.slant;
        1.0 - (-tau * slant).exp()
    }

    /// A Cirrostratus' optical depth straight up along `d`, where it meets the plane `t` metres off at the slope `mu`, at
    /// the wind-frame point `f` (cells) with its veil's `density` there: `tau` x density, its mottles and fibres
    /// moving it. Each detail fades as the texel's footprint along the view (stretched by 1 / mu) nears its own size,
    /// so the far veil is smooth, not a moire; `texel` 0 is one exact direction.
    fn cirrostratus_depth(&self, f: Vec2f, t: f32, mu: f32, density: f32, texel: f32) -> f32 {
        let SheetRecipe::Cirrostratus { tau, mottling, fibres, .. } = self.recipe else {
            return 0.0;
        };
        let look = cirrostratus_look();
        let footprint = t * texel / mu.max(0.02);
        let shown = |size_m: f32| if texel > 0.0 { 1.0 - smoothstep(0.25, 1.0, footprint / size_m.max(1.0e-3)) } else { 1.0 };
        let mut depth = tau * density;
        // Mottles: thicker and thinner patches some half a cell across (its finest octave a quarter cell).
        let mottle = mottling * shown(0.25 * self.cell_m);
        if mottle > 0.0 {
            depth *= (-mottle * look.mottle_depth * (fbm2(f.x + 2.3, f.y + 0.7, CS_MOTTLE_OCTAVES, self.streams.0[2]) - 0.5)).exp();
        }
        // Fibres along the wind: ridged noise stretched along it (its finest lanes CS_FIBRE_FREQ x 4 a cell across).
        let fibre = fibres * shown(self.cell_m / (4.0 * CS_FIBRE_FREQ));
        if fibre > 0.0 {
            let r = ridged2(f.x * CS_FIBRE_FREQ / look.fibre_stretch + 5.1, f.y * CS_FIBRE_FREQ + 1.7, CS_FIBRE_OCTAVES, self.streams.0[3]);
            depth *= (1.0 + fibre * look.fibre_depth * (r - CS_RIDGED_MEAN)).max(0.0);
        }
        depth
    }

    /// The threshold at `lod`, its tables measured with MEASURE_SEED's streams.
    fn threshold(&self, lod: Lod) -> f32 {
        let measure = |octaves: u32, x: f32, y: f32| self.recipe.noise(vec2f(x, y), Lod::whole(octaves), &self.measure).0;
        lod_threshold(&self.remap, lod, &measure)
    }

    /// The sheet along the unit direction `d`: its unfaded cover, its faded alpha, where the ray met its plane, and,
    /// when `lit`, a heap's self-shadow depth toward the sun (4 steps released, 2 `held`, times Shading). Nothing at and
    /// below the horizon, nor along a direction that is not one.
    pub fn shape(&self, d: Vec3f, held: bool, lit: bool) -> Shape {
        self.shape_at(d, held, lit, 0.0)
    }

    /// [`Self::shape`] for a lookup of a bake whose texels are `texel` radians wide (0: one exact direction): a Cirrus'
    /// bundles blur and their fibres go as fine as that texel shows.
    pub fn shape_at(&self, d: Vec3f, held: bool, lit: bool, texel: f32) -> Shape {
        if !(d.y > 0.0) || !d.is_finite() {
            return Shape::default();
        }
        let t = shell_distance(d.y, self.height_m);
        let hit_m = vec2f(d.x * t, d.z * t);
        let p = vec2f(hit_m.x / self.cell_m, hit_m.y / self.cell_m);
        let (fine, broad) = self.recipe.octaves();
        let lod = Lod { fine, broad, detail: self.recipe.lod_weight(detail_at(p.length())) };
        let f = self.frame.of(p);
        let (n, slow, w) = match self.recipe {
            SheetRecipe::Cirrus { .. } => {
                let (n, w) = self.recipe.cirrus_field(f, &self.streams);
                (n, 1.0, w)
            }
            _ => {
                let (n, slow) = self.recipe.noise(f, lod, &self.streams);
                (n, slow, Vec2f::default())
            }
        };
        let a = self.threshold(lod);
        let density = self.remap.density(n, a, f);
        if !(density > 0.0) {
            return Shape { hit_m, height_m: self.height_m, ..Shape::default() };
        }
        let (cover, tau) = match self.recipe {
            SheetRecipe::Cirrus { .. } => {
                // The ray's slope where it meets the plane, on the curved Earth.
                let mu = d.y + t * (1.0 - d.y * d.y) / EARTH_M;
                (self.cirrus_cover(d, t, mu, w, density, texel), 0.0)
            }
            SheetRecipe::Cirrostratus { .. } => {
                let mu = d.y + t * (1.0 - d.y * d.y) / EARTH_M;
                let tau = self.cirrostratus_depth(f, t, mu, density, texel);
                (1.0 - (-tau / mu.max(cirrostratus_look().mu_min)).exp(), tau)
            }
            _ => (self.recipe.opacity(density).clamp(0.0, 1.0), 0.0),
        };
        let tau_sun = if lit { self.self_shadow(p, slow, density, smoothstep(a, a + BODY_SPAN, n), held) } else { 0.0 };
        Shape { cover, alpha: cover * horizon_fade(d.y), tau, tau_sun, hit_m, height_m: self.height_m }
    }

    /// A heap's self-shadow (clouds.rs's 2D march, design §2.3): Shading's share of 3 x the mean density a few steps
    /// toward the sun, read at 4 octaves with the slow factor where the ray landed and the broad table; overhead the
    /// cloud's own density. Its thick body (`body`: how far its noise is above the threshold, 0 to 1 over BODY_SPAN)
    /// deepens a Stratocumulus, so its cells are grey in the middle and bright at the rims; a Cumulus' Flat base deepens
    /// it over its grey band, from the thick core outward; ice has none.
    fn self_shadow(&self, p: Vec2f, slow: f32, density_here: f32, body: f32, held: bool) -> f32 {
        let (shading, flat_base) = match self.recipe {
            SheetRecipe::Cirrus { .. } | SheetRecipe::Cirrostratus { .. } => return 0.0,
            SheetRecipe::Stratocumulus { shading, .. } => (shading, 0.0),
            SheetRecipe::Cumulus { flat_base, .. } => (DEFAULT_SHADING, flat_base),
        };
        let (steps, step) = if held { (2usize, self.shadow_step * 2.0) } else { (SHADOW_STEPS, self.shadow_step) };
        let mean = if step.x == 0.0 && step.y == 0.0 {
            density_here
        } else {
            let a = self.threshold(Lod::whole(self.recipe.octaves().1));
            let mut sum = 0.0f32;
            for i in 1..=steps {
                let f = self.frame.of(p + step * i as f32);
                let n = (self.recipe.step_noise(f, &self.streams) * slow).clamp(0.0, 1.0);
                sum += self.remap.density(n, a, f);
            }
            sum / steps as f32
        };
        // The band starts where it has width: at Flat base 0 (every Stratocumulus) or one too small to move the core in
        // f32 its edges meet, and smoothstep would give 0/0 at the core's density.
        let band_from = FLAT_BASE_CORE - FLAT_BASE_REACH * flat_base;
        let band = if band_from < FLAT_BASE_CORE { flat_base * FLAT_BASE_DEPTH * smoothstep(band_from, FLAT_BASE_CORE, body) } else { 0.0 };
        // A Stratocumulus' thick body is darker than its thin rims; a Cumulus' is its Flat base's band.
        let body = if let SheetRecipe::Stratocumulus { .. } = self.recipe { BODY_DEPTH * body } else { 0.0 };
        (SHADOW_DENSITY * mean + body) * (shading / DEFAULT_SHADING) + band
    }

    /// Its light along `d`, premultiplied by its alpha: its family's law (cloud_light.rs `radiance`), the sun shaded by
    /// `t_above`. A heap's dome reaches its underside through its own depth (its self-shadow), so a thick heap is grey
    /// below and its thin rim bright.
    pub fn radiance(&self, light: &EntryLight, shape: &Shape, d: Vec3f, sun_dir: Vec3f, t_above: f32) -> Vec3f {
        match self.recipe {
            SheetRecipe::Stratocumulus { .. } | SheetRecipe::Cumulus { .. } => {
                let under = UNDER_FLOOR + (1.0 - UNDER_FLOOR) * (-UNDER_PER_DEPTH * shape.tau_sun).exp();
                cloud_light::radiance(&EntryLight { ambient: light.ambient * under, ..*light }, shape, d, sun_dir, t_above)
            }
            SheetRecipe::Cirrus { .. } => {
                // The shared law's ice phase alone (design §2.3), fully lit: the look's flat share of the sun on top
                // (the older ICE_SPREAD) is 0 unless a look round sets it.
                let side = light.sun * (t_above * cirrus_look().spread * light.rows.brightness * shape.alpha);
                cloud_light::radiance(light, shape, d, sun_dir, t_above) + if shape.alpha > 0.0 { side } else { Vec3f::default() }
            }
            SheetRecipe::Cirrostratus { halo, .. } => {
                // Single scattering through thin ice: the sun's light by its phase (lobes and halo), the dome's besides.
                if !(shape.alpha > 0.0) {
                    return Vec3f::default();
                }
                let rows = &light.rows;
                let phase = cirrostratus_phase(d.dot(sun_dir), rows.lining, halo);
                (light.sun * t_above * phase + light.ambient * rows.sky_light) * (rows.brightness * shape.alpha)
            }
        }
    }

    /// What it hides of whatever is behind it along `d`: its unfaded cover (design §2.4).
    pub fn cover_toward(&self, d: Vec3f) -> f32 {
        self.shape(d.normalize(), false, false).cover
    }

    /// What the light pass reads: its own cover, its depth none (a sheet).
    pub fn stand_in(&self, d: Vec3f) -> CoverSample {
        CoverSample { cover: self.cover_toward(d), tau: 0.0 }
    }

    /// Its unfaded opacity at a ground point of its plane (metres east and south): the shade it casts on a lower layer
    /// (cloud_light.rs t_above), from `octaves` of its noise (3 released, 2 held) and the broad table.
    pub fn plane_cover(&self, xz_m: Vec2f, octaves: u32) -> f32 {
        let f = self.frame.of(vec2f(xz_m.x / self.cell_m, xz_m.y / self.cell_m));
        let n = self.recipe.plane_noise(f, octaves.clamp(1, self.recipe.octaves().0), &self.streams);
        let density = self.remap.density(n, self.threshold(Lod::whole(self.recipe.octaves().1)), f);
        if density > 0.0 { self.recipe.opacity(density).clamp(0.0, 1.0) } else { 0.0 }
    }

    /// The remap's density at a point of its plane (cells east and south) at full detail: the drawn sheet's there
    /// (`cover_rises_from_zero` reads it).
    pub fn density_at(&self, p_cells: Vec2f) -> f32 {
        let f = self.frame.of(p_cells);
        let lod = Lod::whole(self.recipe.octaves().0);
        let (n, _) = self.recipe.noise(f, lod, &self.streams);
        self.remap.density(n, self.threshold(lod), f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_classic::{downwind, stand_height_m};
    use crate::dir_from_az_el;

    /// Design §4.3: each sheet at its defaults, from the tail's cover to closed, is nothing at and below the horizon
    /// and along directions that are not ones; above it its cover, alpha, self-shadow and plane cover are finite, the
    /// cover in [0, 1] and never under the alpha, the alpha never negative, and its stand-in is its cover.
    #[test]
    fn the_sheets_are_finite_and_empty_below_the_horizon() {
        let sun = dir_from_az_el(200.0, 30.0);
        for genus in [CloudGenus::Cirrus, CloudGenus::Stratocumulus, CloudGenus::Cumulus] {
            let layer = CloudLayerParams::new(genus, &[], 1);
            for c in [0.0005f32, 0.05, 0.4, 1.0] {
                let sheet = Sheet::build(&layer, genus, c, 1, downwind(30.0), sun, stand_height_m(&layer, genus)).expect("a sheet");
                for el in [0.0f32, -0.5, -10.0, -90.0] {
                    for az in (0..360).step_by(30) {
                        let d = dir_from_az_el(az as f32, el);
                        let s = sheet.shape(d, false, true);
                        assert_eq!((s.cover, s.alpha, s.tau_sun), (0.0, 0.0, 0.0), "{genus:?} at {c}: ({az}, {el})");
                        assert_eq!(sheet.cover_toward(d), 0.0);
                    }
                }
                for d in [Vec3f::default(), vec3f(f32::NAN, 1.0, 0.0), vec3f(f32::INFINITY, 1.0, 0.0)] {
                    assert_eq!(sheet.cover_toward(d), 0.0, "{genus:?}: {d:?}");
                    assert_eq!(sheet.shape(d, false, true).alpha, 0.0, "{genus:?}: {d:?}");
                }
                let mut most = 0.0f32;
                for i in 0..720 {
                    let d = dir_from_az_el(i as f32 * 7.7, 0.2 + (i % 90) as f32);
                    for held in [false, true] {
                        let s = sheet.shape(d, held, true);
                        assert!(s.cover.is_finite() && s.alpha.is_finite() && s.tau_sun.is_finite() && s.tau_sun >= 0.0, "{genus:?} at {c}: {s:?}");
                        assert!((0.0..=1.0).contains(&s.cover) && s.alpha <= s.cover && s.alpha >= 0.0, "{genus:?} at {c}: {s:?}");
                    }
                    let cover = sheet.cover_toward(d);
                    assert_eq!(sheet.stand_in(d).cover, cover);
                    let p = sheet.plane_cover(vec2f(i as f32 * 731.0 - 50_000.0, (i % 37) as f32 * 1777.0 - 30_000.0), 3);
                    assert!((0.0..=1.0).contains(&p), "{genus:?}: plane cover {p}");
                    most = most.max(cover);
                }
                if c >= 0.05 {
                    assert!(most > 0.0, "{genus:?} at {c}: some cloud shows");
                }
            }
        }
    }
}
