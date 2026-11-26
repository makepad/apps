//! The Nimbostratus as a cloud volume (design §2.2's Nimbostratus row and §2.3; plan Task 8 without the joint march of
//! overlapping volumes, which comes later: the Nimbostratus marches as its own shell). A deep deck between its base and
//! its tops, its base torn and its top smooth, a thin broken scud under it at Scud height, and rain under its base.
//!
//! - **Where it stands.** Its footprint grid (cloud_volume.rs, the Cumulus machinery) holds masses one a cell of Size,
//!   a broad fbm, through the cover remap (design §3): at Cover 1 the deck is closed, below it heavy masses with gaps.
//!   A mass is shaped in units of its strength, the footprint noise over the threshold as a share of the strongest
//!   masses' ([`NsShape::span`]): dense inside a rim (NS_EDGE*), narrowing upward as its strength runs out (NS_DOME_*)
//!   and dented by the shape tile (NS_LUMP), so a broken deck is flat heavy masses with lumpy sides, no wall stands the
//!   deck's whole depth at its outline, and a mass is shaped alike whatever share of the plane the Cover solve leaves.
//! - **Thick and thin** ([`NsShape::deck`]): the column channel is Unevenness' field of thick and thin parts (a thick
//!   part hangs lower, stands taller and is denser), and streaks along the wind thin the deck (NS_STREAK_*): through a
//!   thinner deck more light reaches the base, so its underside shows dark masses and lighter streaks.
//! - **The base**: the shape tile read at two scales (NS_RAG_TILE_M, NS_RAG2_TILE_M) drops the base by up to Ragged
//!   base x NS_RAGGED_M, so it is torn into folds and hanging tatters, and the density rises over NS_BASE_RAMP_M from it;
//!   over the top it falls over NS_TOP_RAMP_M, smooth.
//! - **Rows.** Darkness and Density set the extinction (NS_SIGMA_LIGHT + NS_SIGMA_DARK x Darkness^2, design §2.2's
//!   table), Unevenness the thick and thin parts and the streaks, Ragged base the torn base, Scud and Scud height the
//!   scud, Rain curtains the rain, Size the masses' scale.
//! - **CPU and GPU.** The CPU marches the deck alone (the fallback and the light grids' field); the GPU export's arm
//!   (`makepad-cloud-gpu`'s shader, its Nimbostratus section) mirrors [`NsShape::deck`] and adds the fine detail, the
//!   scud and the rain (design §12.12 item 2: the GPU look is the look). The shell both march spans the scud's bottom to
//!   the deck's highest top ([`NsShape::shell_m`]).

use makepad_draw::*;
use crate::cloud_light::CoverSample;
use crate::cloud_sheets::{Streams, WindFrame};
use crate::cloud_volume::Node;
use crate::cloud_volume_gpu::UploadBuilder;
use crate::noise::fbm2;
use crate::volumetric::Tile3;
use crate::NimbostratusParams;

/// Extinction per metre at Density 1 (design §2.2's table): NS_SIGMA_LIGHT + NS_SIGMA_DARK x Darkness^2, 0.083 at the
/// default Darkness 0.6: a mean free path of 12 m, so a closed deck is opaque within a few tens of metres of its base.
pub(crate) const NS_SIGMA_LIGHT: f32 = 0.04;
pub(crate) const NS_SIGMA_DARK: f32 = 0.12;
/// The masses: NS_MASS_FREQ cycles a cell of Size, NS_MASS_OCTAVES octaves, stretched NS_STRETCH along the wind: at the
/// default Size of 8 km masses of 2 to 5 km, several in every view (round 3's one cycle a cell stood one mass and one
/// gap of some 10 km over the viewer: a fog bank round one blue hole).
const NS_MASS_FREQ: f32 = 2.5;
const NS_MASS_OCTAVES: u32 = 4;
const NS_STRETCH: f32 = 1.4;
/// Unevenness' field of thick and thin parts: NS_UNEVEN_FREQ cycles a cell, NS_UNEVEN_OCTAVES octaves, read as
/// u = (column - 0.5) x NS_UNEVEN_GAIN held to [-1, 1] (a 2-octave fbm keeps most of its values within 0.5 +- 0.2).
const NS_UNEVEN_FREQ: f32 = 2.5;
const NS_UNEVEN_OCTAVES: u32 = 2;
pub(crate) const NS_UNEVEN_GAIN: f32 = 3.0;
/// The deck's geometry, metres at a row of 1: a thick part's base hangs NS_UNEVEN_M lower (a thin part's stands as much
/// higher), a thin part's top stands up to NS_UNEVEN_TOP_M lower; Ragged base drops the base by up to NS_RAGGED_M; the
/// density rises over NS_BASE_RAMP_M from the base and falls over NS_TOP_RAMP_M under the top; a thin part keeps
/// 1 - NS_THIN_SHARE x Unevenness of the density. A deck thinner by a half passes about twice the light to its base
/// (the slab law's T_d), which is what draws the underside's darker and lighter parts: at round 1's 0.5 and no lower top
/// the base's luminance varied by 0.14 of its mean (the owner's photos 0.26-1.0).
pub(crate) const NS_UNEVEN_M: f32 = 500.0;
pub(crate) const NS_UNEVEN_TOP_M: f32 = 1500.0;
pub(crate) const NS_RAGGED_M: f32 = 600.0;
pub(crate) const NS_BASE_RAMP_M: f32 = 80.0;
pub(crate) const NS_TOP_RAMP_M: f32 = 400.0;
pub(crate) const NS_THIN_SHARE: f32 = 0.8;
/// The torn base's noise: the shape tile read as tiles of NS_RAG_TILE_M and NS_RAG2_TILE_M (Perlin octaves of 1.5 km to
/// 375 m and of 450 to 110 m), each over its mean in its standard deviations, mixed 0.65 / 0.45 and over NS_RAG_SPREAD
/// held to [-1, 1].
pub(crate) const NS_RAG_TILE_M: f32 = 6000.0;
pub(crate) const NS_RAG2_TILE_M: f32 = 1800.0;
pub(crate) const NS_RAG_SPREAD: f32 = 1.2;
/// The masses' law, in units of a mass's strength s = (n - threshold) / span, at most 1: the footprint noise over the
/// threshold as a share of the strongest masses' (span = the noise's NS_HI_QUANTILE quantile less the threshold, at
/// least NS_SPAN_LEAST), so a mass is shaped alike whatever share of the plane the Cover solve leaves. (In noise units,
/// round 3 found the masses at Cover 0.6 about a hundredth over the threshold under an edge of 0.15, lumps of 0.04 and a
/// dome of 0.17: veils of optical depth 1 lit through, white from below, a thinned sheet rather than heavy masses.)
/// With o = (1 - Cover)^NS_OPEN_POWER (1 at no cover, 0.03 at 0.6, 0 closed), sc = s held to [0, 1] and h the height
/// through the deck:
/// - **Rim.** Its density rises over NS_EDGE + NS_EDGE_OPEN x o of s from its outline, its outline torn by the lumps'
///   dents: a soft grey fringe that the light law keeps grey (a broken deck's mass is lit as a slab of the deck's
///   column), never a lit-through white wisp.
/// - **Top.** A lens: from the deck's base at its rim to the deck's top where sc reaches dome = NS_DOME_CLOSED +
///   NS_DOME_OPEN x o (as sqrt(sc / dome)): toward a closed deck the masses stand the deck's depth over nearly all their
///   footprint; at a low Cover they are few, small and flat (at 0.1 at most some 600 m), so they hide no more sky from
///   the side than the Cover solve makes up with their share of the plane (`cover_rises_from_zero`).
/// - **Base.** It sags with the strength: a weak part's base stands NS_SAG_M x (1 - sc) over the strongest's, so a mass
///   bulges downward, its middle hanging lowest, and a closed deck's base has heavy masses hanging between higher,
///   thinner parts; under it the billow tile's lumps (NS_LUMPS_*) hang up to NS_LUMPS_M, and the rag tears it.
/// - **Density.** NS_WEAK + (1 - NS_WEAK) x sc of the deck's: the strong masses dense, the weak parts between them thin,
///   which (with the sag) is what draws a closed base's dark masses and lighter gaps (round 3's closed base, its density
///   the same everywhere, was a pale smear, std/mean 0.22 against the photos' 0.23-1.0).
/// - **Lumps.** Its sides are dented by NS_LUMP of s a standard deviation of the shape tile read as a tile of
///   NS_LUMP_TILE_M where that stands under its mean: the dents only carve, so no mass reaches past its own footprint.
/// At Cover 1 the threshold is the noise's floor less the remap's edge, so the weakest point is a third of the span over
/// it and the deck stays closed, its top whole.
pub(crate) const NS_HI_QUANTILE: f32 = 63.0 / 64.0;
pub(crate) const NS_SPAN_LEAST: f32 = 0.1;
pub(crate) const NS_EDGE: f32 = 0.35;
pub(crate) const NS_EDGE_OPEN: f32 = 0.65;
pub(crate) const NS_DOME_CLOSED: f32 = 0.1;
pub(crate) const NS_DOME_OPEN: f32 = 3.9;
pub(crate) const NS_OPEN_POWER: f32 = 4.0;
pub(crate) const NS_LUMP: f32 = 0.08;
pub(crate) const NS_LUMP_TILE_M: f32 = 3000.0;
pub(crate) const NS_SAG_M: f32 = 1200.0;
/// (0.35 in round 4; the GPU's soft base since round 5 needs the masses' own contrast to keep a closed base as uneven as
/// the photos)
pub(crate) const NS_WEAK: f32 = 0.25;
/// The base's lumps (the GPU's fine detail): the billow tile (balls of a half, a quarter and an eighth of it) read flat
/// in the warped wind frame as a tile of NS_LUMPS_TILE_M (balls of 1.5 km to 375 m), over its mean in its spreads,
/// smoothstep(-0.5, 1.5), hang the base down by up to NS_LUMPS_M: the heavy lumps of a Nimbostratus' underside.
pub(crate) const NS_LUMPS_TILE_M: f32 = 3000.0;
pub(crate) const NS_LUMPS_M: f32 = 250.0;
/// The streaks: the shape tile read as a tile of NS_STREAK_TILE_M across the wind and NS_STREAK_STRETCH times that along
/// it, in the warped wind frame; where it stands NS_STREAK_FROM to NS_STREAK_TO standard deviations over its mean the
/// deck keeps 1 - NS_STREAK_SHARE x min(1, 2 Unevenness) of its density, through its whole depth: a column a third as
/// deep passes three times the light to its base (the slab law's T_d), so the streaks read lighter from below (round 2's
/// 5 km tile over 0.7 to 1.5 spreads thinned a fifth of the sky by under half and drew none; round 4's 0.8 of the density
/// stretched five times along the wind drew the overhead as a swirl of smears).
pub(crate) const NS_STREAK_TILE_M: f32 = 2500.0;
pub(crate) const NS_STREAK_STRETCH: f32 = 2.5;
pub(crate) const NS_STREAK_SHARE: f32 = 0.5;
pub(crate) const NS_STREAK_FROM: f32 = 0.4;
pub(crate) const NS_STREAK_TO: f32 = 1.2;
/// The wind's frame the masses' noises are read in (`NsShape::wind_at`): the along coordinate moved by NS_WARP_M of the
/// shape tile read as a tile of NS_WARP_TILE_M, so the rag, the lumps and the streaks are dragged and torn along the
/// wind; the rag stretched NS_RAG_STRETCH along it and the lumps NS_LUMP_STRETCH (round 2's round lumps and blobs read
/// as bubbles; round 3's warp of 1500 m smeared the base into marbled swirls; round 4's lumps stretched 2.5 read as smears).
pub(crate) const NS_WARP_M: f32 = 600.0;
pub(crate) const NS_WARP_TILE_M: f32 = 4000.0;
pub(crate) const NS_RAG_STRETCH: f32 = 2.0;
pub(crate) const NS_LUMP_STRETCH: f32 = 1.5;
/// The tiles' mean and spread are read at NS_SPREAD_READS points.
const NS_SPREAD_READS: u32 = 4096;
/// The far deck's depth in the stand-in is the depth its cover stands for, its cover held under this (the light pass's
/// sheets' SHEET_COVER_MAX).
const NS_FAR_COVER_MAX: f32 = 0.999;
/// The scud: a band NS_SCUD_DEPTH_M deep round Scud height, its tatters reaching NS_SCUD_REACH_M past it; it stands at
/// least NS_SCUD_GAP_M under the deck's highest base (cloud_slabs.rs' SCUD_GAP_M) and not under NS_SCUD_LOWEST_M
/// (the Scud height row's lowest).
pub(crate) const NS_SCUD_DEPTH_M: f32 = 160.0;
const NS_SCUD_REACH_M: f32 = 120.0;
const NS_SCUD_GAP_M: f32 = 50.0;
const NS_SCUD_LOWEST_M: f32 = 150.0;
/// The footprint's part in its quantile tables' fingerprint (the slab's mask and scud are 0 and 1).
pub(crate) const NS_FOOTPRINT_PART: u32 = 3;
/// The recipe's id in the footprint grid's key (the Cumulus' is 3).
pub(crate) const NS_RECIPE: f32 = 5.0;

/// A Nimbostratus' extinction per metre at its Darkness and Density.
pub(crate) fn sigma(b: &NimbostratusParams) -> f32 {
    (NS_SIGMA_LIGHT + NS_SIGMA_DARK * b.darkness * b.darkness) * b.density
}

/// The footprint recipe at a point `f` of the wind frame (cells of Size): the masses' noise, Unevenness' field (the
/// column channel), no warp and full strength (the grid's carried channels the Cumulus reads).
pub(crate) fn recipe_at(f: Vec2f, s: &Streams) -> Node {
    let g = vec2f(f.x / NS_STRETCH, f.y);
    let n = fbm2(g.x * NS_MASS_FREQ + 0.37, g.y * NS_MASS_FREQ + 0.71, NS_MASS_OCTAVES, s.0[0]);
    let column = fbm2(g.x * NS_UNEVEN_FREQ + 4.1, g.y * NS_UNEVEN_FREQ + 2.3, NS_UNEVEN_OCTAVES, s.0[1]);
    [n, column, 0.0, 0.0, 1.0]
}

/// The recipe at a ground point (metres east and south), as the footprint grid holds it.
pub(crate) fn recipe_m(frame: &WindFrame, streams: &Streams, cell_m: f32, x: f32, z: f32) -> Node {
    recipe_at(frame.of(vec2f(x / cell_m, z / cell_m)), streams)
}

/// A Nimbostratus layer's deck, scud and rain (its rows), its wind's way and the tiles' spreads its noises read round.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NsShape {
    /// The deck's base and tops, metres, as the rows give them (a thick part hangs lower, a thin one stands lower).
    pub(crate) base_m: f32,
    pub(crate) top_m: f32,
    pub(crate) ragged: f32,
    pub(crate) unevenness: f32,
    /// The scud's height (0: no scud) and its row.
    pub(crate) scud_m: f32,
    pub(crate) scud: f32,
    pub(crate) rain: f32,
    pub(crate) darkness: f32,
    /// The way the wind blows (east, south): the streaks lie along it.
    pub(crate) down: Vec2f,
    /// The masses' law at the layer's Cover (NS_EDGE*, NS_DOME_*): a mass's rim, and how much of its strength its height
    /// uses up at the deck's top.
    pub(crate) edge: f32,
    pub(crate) dome: f32,
    /// The footprint noise's NS_HI_QUANTILE quantile (the strongest masses'), set by the build from the recipe's table.
    pub(crate) q_hi: f32,
    /// The shape tile's mean and standard deviation, which the deck's noises read round, and the detail tile's (the
    /// GPU's wisps and scud).
    pub(crate) shape_mean: f32,
    pub(crate) shape_std: f32,
    pub(crate) detail_mean: f32,
    pub(crate) detail_std: f32,
    /// The billow tile's mean and standard deviation (the base's lumps).
    pub(crate) billow_mean: f32,
    pub(crate) billow_std: f32,
}

/// A tile's mean and standard deviation over NS_SPREAD_READS fixed points.
fn spread(tile: &Tile3) -> (f32, f32) {
    let unit = |i: u32, k: u32| (crate::noise::hash_u32(i.wrapping_mul(0x9e37_79b9) ^ k.wrapping_mul(0x85eb_ca6b)) >> 8) as f32 * (1.0 / 16_777_216.0);
    let (mut sum, mut sum2) = (0.0f64, 0.0f64);
    for i in 0..NS_SPREAD_READS {
        let v = tile.sample(unit(i, 1), unit(i, 2), unit(i, 3)) as f64;
        sum += v;
        sum2 += v * v;
    }
    let n = NS_SPREAD_READS as f64;
    let mean = sum / n;
    (mean as f32, (sum2 / n - mean * mean).max(1.0e-8).sqrt() as f32)
}

impl NsShape {
    /// The layer's block `b` at `cover` in the wind blowing `down`, its noises read round the shape tile `shape`'s mean and
    /// spread, the detail tile `detail`'s and the billow tile `billow`'s (NS_SPREAD_READS reads each at fixed points:
    /// once a build, well under a millisecond). Its `q_hi` is the build's to set (1 until then).
    pub(crate) fn of(b: &NimbostratusParams, cover: f32, down: Vec2f, shape: &Tile3, detail: &Tile3, billow: &Tile3) -> NsShape {
        let base_m = b.base_m;
        let top_m = b.tops_m.max(base_m + 1.0);
        let scud_m = b.scud_height_m.min(base_m - NS_SCUD_GAP_M);
        let scud_m = if b.scud > 0.0 && scud_m >= NS_SCUD_LOWEST_M { scud_m } else { 0.0 };
        let (shape_mean, shape_std) = spread(shape);
        let (detail_mean, detail_std) = spread(detail);
        let (billow_mean, billow_std) = spread(billow);
        let open = (1.0 - cover.clamp(0.0, 1.0)).powf(NS_OPEN_POWER);
        let (edge, dome) = (NS_EDGE + NS_EDGE_OPEN * open, NS_DOME_CLOSED + NS_DOME_OPEN * open);
        NsShape { base_m, top_m, ragged: b.ragged_base, unevenness: b.unevenness, scud_m, scud: b.scud, rain: b.rain_curtains, darkness: b.darkness, down, edge, dome, q_hi: 1.0, shape_mean, shape_std, detail_mean, detail_std, billow_mean, billow_std }
    }

    /// The lowest the deck's base reaches (a thick part's, torn as far as Ragged base drops it, its lumps hanging).
    fn lowest_base_m(&self) -> f32 {
        self.base_m - self.unevenness * NS_UNEVEN_M - self.ragged * NS_RAGGED_M - NS_LUMPS_M
    }

    /// The shell the volume marches, metres above the ground: from the scud's bottom (or the deck's lowest base, never
    /// under 1 m) to the deck's highest top.
    pub(crate) fn shell_m(&self) -> (f32, f32) {
        let mut low = self.lowest_base_m();
        if self.scud_m > 0.0 {
            low = low.min(self.scud_m - 0.5 * NS_SCUD_DEPTH_M - NS_SCUD_REACH_M);
        }
        (low.max(1.0), self.top_m)
    }

    /// The strength's unit at the footprint's `threshold`: the strongest masses' noise over it, at least NS_SPAN_LEAST.
    pub(crate) fn span(&self, threshold: f32) -> f32 {
        (self.q_hi - threshold).max(NS_SPAN_LEAST)
    }

    /// The shape tile at `p` read as a tile of `tile_m` metres from `place`, over its mean in its standard deviations.
    fn shape_z(&self, shape: &Tile3, p: Vec3f, tile_m: f32, place: Vec3f) -> f32 {
        let s = 1.0 / tile_m;
        (shape.sample(p.x * s + place.x, p.y * s + place.y, p.z * s + place.z) - self.shape_mean) / self.shape_std
    }

    /// The wind's frame at `p`, warped along the wind (NS_WARP_M): (along, height, across), where the masses' noises are
    /// read. The GPU's arm mirrors it (its `ns_wind`).
    fn wind_at(&self, shape: &Tile3, p: Vec3f) -> Vec3f {
        let along = p.x * self.down.x + p.z * self.down.y;
        let across = p.z * self.down.x - p.x * self.down.y;
        let warp = NS_WARP_M * self.shape_z(shape, vec3f(along, p.y, across), NS_WARP_TILE_M, vec3f(0.57, 0.13, 0.79));
        vec3f(along + warp, p.y, across)
    }

    /// The deck's density (0 to 1) at `p` under Unevenness' field `column` in a mass of strength `s` (its footprint noise
    /// over the threshold in the unit `span`, at most 1), the masses' law above: the strength dented by the lumps (they
    /// only carve), the lens' top, the base sagging with the strength and torn by the rag (the shape tile at two scales),
    /// the ramps up from the base and down from the top, the thin parts and the streaks thinner, the weak parts thinner,
    /// the rim. The rag, the lumps and the streaks read in the warped wind frame. The GPU's arm mirrors it (its `ns_deck`
    /// and `ns_dens`) and hangs the billow's lumps under its base, the fine detail, as it adds the wisps (a billow read
    /// here, a large tile read across the grids' points, made a closed deck's grids and bake six to fourteen times
    /// slower: a 1024 bake took 38 s).
    pub(crate) fn deck(&self, p: Vec3f, column: f32, s: f32, shape: &Tile3) -> f32 {
        let u = ((column - 0.5) * NS_UNEVEN_GAIN).clamp(-1.0, 1.0);
        let low0 = self.base_m - self.unevenness * NS_UNEVEN_M * u;
        let top0 = self.top_m - self.unevenness * NS_UNEVEN_TOP_M * 0.5 * (1.0 - u);
        let drop = self.ragged * NS_RAGGED_M;
        if !(p.y < top0 && p.y > low0 - drop - NS_LUMPS_M) {
            return 0.0;
        }
        let wq = self.wind_at(shape, p);
        let sl = s + NS_LUMP * self.shape_z(shape, vec3f(wq.x / NS_LUMP_STRETCH, wq.y, wq.z), NS_LUMP_TILE_M, vec3f(0.61, 0.29, 0.93)).min(0.0);
        if !(sl > 0.0) {
            return 0.0;
        }
        let sc = sl.min(1.0);
        let top = low0 + (top0 - low0) * (sc / self.dome).min(1.0).sqrt();
        let rq = vec3f(wq.x / NS_RAG_STRETCH, wq.y, wq.z);
        let rag = ((0.65 * self.shape_z(shape, rq, NS_RAG_TILE_M, vec3f(0.31, 0.17, 0.53)) + 0.45 * self.shape_z(shape, rq, NS_RAG2_TILE_M, vec3f(0.83, 0.41, 0.07))) / NS_RAG_SPREAD).clamp(-1.0, 1.0);
        let base = low0 + NS_SAG_M * (1.0 - sc) - drop * (0.5 + 0.5 * rag);
        if !(p.y > base && p.y < top) {
            return 0.0;
        }
        let ramp = smoothstep(0.0, NS_BASE_RAMP_M, p.y - base);
        let crown = 1.0 - smoothstep(top - NS_TOP_RAMP_M, top, p.y);
        let thin = 1.0 - NS_THIN_SHARE * self.unevenness * 0.5 * (1.0 - u);
        let streak_at = vec3f(wq.x / NS_STREAK_STRETCH, 0.0, wq.z);
        let streak = smoothstep(NS_STREAK_FROM, NS_STREAK_TO, self.shape_z(shape, streak_at, NS_STREAK_TILE_M, vec3f(0.19, 0.47, 0.71)));
        let streaks = 1.0 - NS_STREAK_SHARE * (2.0 * self.unevenness).min(1.0) * streak;
        ramp * crown * thin * streaks * (NS_WEAK + (1.0 - NS_WEAK) * sc) * smoothstep(0.0, self.edge, sl)
    }

    /// Its rows and the engine's numbers the GPU export's Nimbostratus arm reads (design §12.3: each module writes its
    /// own): `ns_a` (base, tops, Ragged base, Unevenness), `ns_b` (the scud's height or 0, Scud, Rain curtains,
    /// Darkness), `ns_c` (the shape tile's mean, the scud's depth and reach, the shape tile's spread), `ns_d` (the detail
    /// tile's mean and spread, the layer's effective `cover` (the far deck's), the rim), `ns_k` (NS_UNEVEN_M,
    /// NS_RAGGED_M, NS_BASE_RAMP_M, NS_TOP_RAMP_M), `ns_k2` (NS_RAG_TILE_M, NS_RAG_SPREAD, NS_UNEVEN_GAIN, NS_THIN_SHARE),
    /// `ns_k3` (the dome at its Cover, NS_LUMP, NS_LUMP_TILE_M, NS_UNEVEN_TOP_M), `ns_k4` (NS_STREAK_TILE_M,
    /// NS_STREAK_STRETCH, NS_STREAK_SHARE, NS_RAG2_TILE_M), `ns_k5` (NS_STREAK_FROM, NS_STREAK_TO, the strength's unit at
    /// the footprint's `threshold`, -), `ns_k6` (NS_WARP_M, NS_WARP_TILE_M, NS_RAG_STRETCH, NS_LUMP_STRETCH), `ns_k7` (the
    /// billow tile's mean and spread, NS_LUMPS_TILE_M, NS_LUMPS_M), `ns_k8` (NS_SAG_M, NS_WEAK, -, -).
    pub(crate) fn gpu_layout(&self, out: &mut UploadBuilder, cover: f32, threshold: f32) {
        out.uniform("ns_a", [self.base_m, self.top_m, self.ragged, self.unevenness]);
        out.uniform("ns_b", [self.scud_m, self.scud, self.rain, self.darkness]);
        out.uniform("ns_c", [self.shape_mean, NS_SCUD_DEPTH_M, NS_SCUD_REACH_M, self.shape_std]);
        out.uniform("ns_d", [self.detail_mean, self.detail_std, cover.clamp(0.0, 1.0), self.edge]);
        out.uniform("ns_k", [NS_UNEVEN_M, NS_RAGGED_M, NS_BASE_RAMP_M, NS_TOP_RAMP_M]);
        out.uniform("ns_k2", [NS_RAG_TILE_M, NS_RAG_SPREAD, NS_UNEVEN_GAIN, NS_THIN_SHARE]);
        out.uniform("ns_k3", [self.dome, NS_LUMP, NS_LUMP_TILE_M, NS_UNEVEN_TOP_M]);
        out.uniform("ns_k4", [NS_STREAK_TILE_M, NS_STREAK_STRETCH, NS_STREAK_SHARE, NS_RAG2_TILE_M]);
        out.uniform("ns_k5", [NS_STREAK_FROM, NS_STREAK_TO, self.span(threshold), 0.0]);
        out.uniform("ns_k6", [NS_WARP_M, NS_WARP_TILE_M, NS_RAG_STRETCH, NS_LUMP_STRETCH]);
        out.uniform("ns_k7", [self.billow_mean, self.billow_std, NS_LUMPS_TILE_M, NS_LUMPS_M]);
        out.uniform("ns_k8", [NS_SAG_M, NS_WEAK, 0.0, 0.0]);
    }
}

/// A Nimbostratus' horizon fade of its alpha (the compositor's, in place of a flat entry's `horizon_fade`): none. Its
/// GPU arm carries the deck past its march's range to the horizon, and the ground side under it is continuous with
/// the sky there (design §12.14 item 2), so no clear horizon shows through: a flat entry's fade (0.12 of sin el) showed
/// a bright band under every overcast, the round-2 fade over its last 0.0005 a one-texel line at 8K.
pub(crate) fn horizon_fade(sin_el: f32) -> f32 {
    if sin_el > 0.0 { 1.0 } else { 0.0 }
}

/// The far deck at the layer's `cover` along a line of sight `cells` cells of Size long through its heights: past its
/// march's range the GPU arm carries the deck on to the horizon, met as often as a line that long through a deck of
/// that Cover meets a mass, 1 - (1 - cover)^(1 + cells) (a closed deck always). A look whose base lies past the range
/// (the light pass's and the facts' stand-in) and a sun's ray meeting the base past the sun grid (its shadow on a
/// lower layer) see that cover.
pub(crate) fn far_cover(cover: f32, cells: f32) -> f32 {
    1.0 - (1.0 - cover.clamp(0.0, 1.0)).powf(1.0 + cells.max(0.0))
}

/// [`far_cover`] as a stand-in's look: the cover, at the depth it stands for.
pub(crate) fn far_look(cover: f32, cells: f32) -> CoverSample {
    let cover = far_cover(cover, cells);
    CoverSample { cover, tau: -(1.0 - cover.min(NS_FAR_COVER_MAX)).ln() }
}

use crate::patterns::smooth as smoothstep;
