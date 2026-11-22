//! The Slab class (design §2.1, §4.3): a layer of real optical depth on a plane at its base, met on the curved Earth
//! as a sheet is (cloud_sheets.rs). A mask from its recipe's noise, through the cover remap, says where it is; tau
//! there is its depth, and its opacity 1 - e^(-tau n / mu), mu = max(sin el, 0.1), so it closes toward the horizon; a
//! Stratus' opacity is sqrt(n) (1 - e^(-tau / mu)) instead, the share of the texel its sheet covers, so its edge thins
//! out softly across the mask's whole edge band. Stratus and Nimbostratus: the Nimbostratus' scud is a dark
//! ragged sheet under it at Scud height (at most SCUD_GAP_M under its base, none when there is no room), drifting
//! twice as far, and composited at its own height in the stack (cloud_stack.rs's walk); its rain curtains hang in its
//! own alpha as darker bands low round the horizon.
//!
//! **Cover.** The opacity of a deep slab is near 1 wherever its mask is above a few hundredths, so with the sheets'
//! threshold it would hide the share of its plane with any cloud, well over its cover. Its threshold instead puts the
//! mask's knee, where the slab seen straight up is half opaque, on the cover's quantile, so the share of the plane it
//! hides is its Cover (design §3.1: the 0.5 contour encloses about c). Over the last TAIL_SHARE of cover the knee moves
//! down to the edge's foot, so at Cover 1 the threshold is the sheets' -w and the slab closes.

use makepad_draw::*;
use crate::cloud_cover::{fingerprint, quantile_set, CoverRemap, MEASURE_SEED, TAIL_SHARE};
use crate::cloud_light::{self, CoverSample, EntryLight, Shape};
use crate::cloud_sheets::{detail_at, horizon_fade, lod_fbm, lod_threshold, plane_hit, Lod, Streams, WindFrame};
use crate::noise::{fbm2, hash_u32, value2};
use crate::sky::luminance;
use crate::{cloud_layer_seed, CloudGenus, CloudLayerParams, CLOUD_NIMBOSTRATUS_SCUD_HEIGHT_RANGE};

/// A slab's edge width (it has no Softness row), and the lowest mu its depth is seen through.
const SLAB_EDGE: f32 = 0.15;
/// The cover from which the knee stands on the cover's quantile (`knee_shift`); below it the knee eases back to the
/// remap's own threshold at TAIL_SHARE, where the tail gate thins the clouds.
const KNEE_FROM: f32 = 4.0 * TAIL_SHARE;
const MIN_MU: f32 = 0.1;
const SLAB_OCTAVES: u32 = 3;
const SLAB_BROAD_OCTAVES: u32 = 2;
/// The Stratus' Ragged edges and Mottling, and the Nimbostratus' Unevenness and Ragged base: how far each moves.
/// Mottling and Ragged base scale tau by e^(-amount x noise): a mottle thickens the sheet as far as it thins it, and a
/// torn patch thins it toward a bright gap. The Stratus' mottle is faint: at the default Mottling its depth moves by a
/// few percent, and so does its light.
const RAGGED_AMOUNT: f32 = 0.5;
const MOTTLE_DEPTH: f32 = 1.0;
const UNEVEN_DEPTH: f32 = 2.0;
const RAGGED_THIN: f32 = 2.0;
/// Their noises' frequencies, in cycles a cell, and the mottle's octaves: soft patches some 300 m across at the default
/// Size, with finer ones in them.
const MOTTLE_FREQ: f32 = 20.0;
const MOTTLE_OCTAVES: u32 = 3;
/// The Stratus' ragged edges: their noise's cycles a cycle of its mask.
const RAGGED_FREQ: f32 = 2.5;
const UNEVEN_FREQ: f32 = 1.5;
/// The Stratus' mask: cycles a cell (at the default Size its patches are some 750 m across, flat with soft edges) and
/// its octaves, and its optical depth: STRATUS_TAU_THIN + STRATUS_TAU_PER_DENSITY x Density, a grey layer at the default
/// Density 0.35 that still shows where the sun is.
const STRATUS_MASK_FREQ: f32 = 8.0;
const STRATUS_OCTAVES: u32 = 2;
const STRATUS_TAU_THIN: f32 = 2.0;
const STRATUS_TAU_PER_DENSITY: f32 = 65.0;
/// How much brighter a Stratus is toward the sun than its diffuse light (SUN_GLOW at the sun), and how narrow that
/// patch is: ((1 + cos) / 2)^SUN_LEAN of the angle from the sun.
const SUN_GLOW: f32 = 2.0;
const SUN_LEAN: i32 = 16;
/// How far Streaks stretches the Stratus along the wind (1 + this x Streaks): a little, so its patches never fan out
/// toward the horizon.
const STRATUS_STRETCH: f32 = 1.5;
/// Where the Stratus' patches merge into their mean, in cycles of its mask from the point overhead: from the first
/// on, fully at the second, where the patches are smaller than a texel and would only alias into streaks.
const MERGE_FROM: f32 = 3.0;
const MERGE_TO: f32 = 12.0;
/// The mottle's finest octave (4 x MOTTLE_FREQ cycles a cell) over a mask's finest at one cycle a cell (4): its detail
/// fades at the same count of its own cycles as the mask's does, so it never sparkles toward the horizon.
const MOTTLE_FADE: f32 = (1u32 << (MOTTLE_OCTAVES - 1)) as f32 * MOTTLE_FREQ / (1u32 << (SLAB_OCTAVES - 1)) as f32;
/// The scud: its share of its plane per unit of Scud (times the layer's cover), its depth, its cell as a share of the
/// slab's, and how dark it shows against the light it gets.
const SCUD_SHARE: f32 = 0.8;
const SCUD_TAU: f32 = 3.0;
const SCUD_CELL_SHARE: f32 = 1.0 / 3.0;
const SCUD_LIGHT: f32 = 0.5;
/// The least gap between the scud and its slab's base: a Scud height closer to the base, or over it, draws the scud
/// that far under the base, and a base too low to leave the lowest Scud height under it draws none. clamp() holds the
/// stored Scud height to the same gap.
pub(crate) const SCUD_GAP_M: f32 = 50.0;
/// The Nimbostratus' optical depth: NIMBO_TAU_LIGHT + NIMBO_TAU_PER_DARKNESS x Darkness^2 (168 at the default 0.6),
/// and the cool grey its light turns: the multiply scattered light under a rain deck is the dome's more than the
/// sun's: NIMBO_COOL of the way to the blue-grey NIMBO_TINT at the same luminance.
const NIMBO_TAU_LIGHT: f32 = 60.0;
const NIMBO_TAU_PER_DARKNESS: f32 = 300.0;
const NIMBO_TINT: Vec3f = Vec3f { x: 0.88, y: 1.0, z: 1.2 };
const NIMBO_COOL: f32 = 0.85;
/// Rain curtains: bands round the horizon, the sine of the elevation they hang below, and how much they darken.
const CURTAIN_BANDS: f32 = 64.0;
const CURTAIN_TOP: f32 = 0.45;
const CURTAIN_DARKEN: f32 = 0.7;

/// A slab type's recipe and the rows it reads.
#[derive(Clone, Copy, Debug)]
pub enum SlabRecipe {
    Stratus { tau: f32, mottling: f32, aspect: f32, ragged: f32 },
    Nimbostratus { tau: f32, unevenness: f32, ragged: f32, curtains: f32 },
}

impl SlabRecipe {
    /// The fields that change the mask's spread: the fingerprint's (design §3.1).
    fn shape_fields(&self) -> Vec<f32> {
        match *self {
            SlabRecipe::Stratus { aspect, ragged, .. } => vec![aspect, ragged],
            SlabRecipe::Nimbostratus { .. } => Vec::new(),
        }
    }

    /// Its opacity at mask density m under depth `deck`, seen at mu, and the depth there, deck x m: the Nimbostratus'
    /// opacity is that depth's; the Stratus' sheet covers sqrt(m) of the texel at its full depth, so its edge thins out
    /// over the mask's whole edge band, a soft fringe rather than a bright rim.
    fn opacity(&self, deck: f32, m: f32, mu: f32) -> (f32, f32) {
        let tau = deck * m;
        let cover = match self {
            SlabRecipe::Stratus { .. } => m.sqrt() * (1.0 - (-deck / mu).exp()),
            SlabRecipe::Nimbostratus { .. } => 1.0 - (-tau / mu).exp(),
        };
        (cover.clamp(0.0, 1.0), tau)
    }

    /// The mask density at which the slab, seen straight up, is half opaque (1 when it never is).
    fn half_opaque(&self) -> f32 {
        let tau = self.base_tau().max(f32::MIN_POSITIVE);
        match self {
            SlabRecipe::Stratus { .. } => {
                let share = (0.5 / (1.0 - (-tau).exp())).min(1.0);
                share * share
            }
            SlabRecipe::Nimbostratus { .. } => std::f32::consts::LN_2 / tau,
        }
    }

    /// Its tau before the mask.
    fn base_tau(&self) -> f32 {
        match *self {
            SlabRecipe::Stratus { tau, .. } | SlabRecipe::Nimbostratus { tau, .. } => tau,
        }
    }

    /// Cycles of the mask a cell: the distance LOD and the merge count in them.
    fn mask_freq(&self) -> f32 {
        match self {
            SlabRecipe::Stratus { .. } => STRATUS_MASK_FREQ,
            SlabRecipe::Nimbostratus { .. } => 1.0,
        }
    }

    /// The mask's noise n in [0, 1] at a point `f` of the wind frame (cells).
    fn mask(&self, f: Vec2f, lod: Lod, s: &Streams) -> f32 {
        match *self {
            SlabRecipe::Stratus { aspect, ragged, .. } => {
                // Streaks stretch the mask along the wind.
                let root = aspect.sqrt();
                let q = vec2f(f.x / root, f.y * root) * STRATUS_MASK_FREQ;
                // Its broad octaves only: a gentle slope, so its edge is soft.
                let mut n = lod_fbm(q, lod.capped(STRATUS_OCTAVES), s.0[0]);
                // Ragged edges: a finer noise tears the band where the mask crosses its threshold.
                if ragged > 0.0 {
                    n += ragged * RAGGED_AMOUNT * (fbm2(q.x * RAGGED_FREQ + 0.7, q.y * RAGGED_FREQ + 2.9, 2, s.0[1]) - 0.5);
                }
                n.clamp(0.0, 1.0)
            }
            SlabRecipe::Nimbostratus { .. } => lod_fbm(f, lod, s.0[0]),
        }
    }

    /// tau at a point `distance_cells` from the point overhead, before the mask: the Stratus' Mottling over its mottle
    /// (fading with distance as the mask's fine octaves do, at its own frequency), the Nimbostratus' slow Unevenness
    /// and its Ragged base's torn thin patches.
    fn depth(&self, f: Vec2f, distance_cells: f32, s: &Streams) -> f32 {
        match *self {
            SlabRecipe::Stratus { tau, mottling, aspect, .. } => {
                let mottling = mottling * detail_at(distance_cells * MOTTLE_FADE);
                if !(mottling > 0.0) {
                    return tau;
                }
                let root = aspect.sqrt();
                let q = vec2f(f.x / root, f.y * root);
                tau * (-mottling * MOTTLE_DEPTH * (fbm2(q.x * MOTTLE_FREQ + 2.3, q.y * MOTTLE_FREQ + 0.7, MOTTLE_OCTAVES, s.0[2]) - 0.5)).exp()
            }
            SlabRecipe::Nimbostratus { tau, unevenness, ragged, .. } => {
                let mut t = tau;
                if unevenness > 0.0 {
                    t *= 1.0 + unevenness * UNEVEN_DEPTH * (fbm2(f.x * UNEVEN_FREQ, f.y * UNEVEN_FREQ, 2, s.0[1]) - 0.5);
                }
                if ragged > 0.0 {
                    t *= (-ragged * RAGGED_THIN * smoothstep(0.5, 0.8, fbm2(f.x * 2.7 + 5.1, f.y * 2.7 + 1.7, 2, s.0[2]))).exp();
                }
                t.max(0.0)
            }
        }
    }
}

/// The Nimbostratus' scud: a thin dark sheet at Scud height, under its slab's base.
struct Scud {
    remap: CoverRemap,
    height_m: f32,
    cell_m: f32,
    frame: WindFrame,
}

impl Scud {
    fn shape(&self, d: Vec3f, streams: &Streams, measure: &Streams) -> Shape {
        let hit_m = plane_hit(d, self.height_m);
        let p = vec2f(hit_m.x / self.cell_m, hit_m.y / self.cell_m);
        let lod = Lod { fine: SLAB_OCTAVES, broad: SLAB_BROAD_OCTAVES, detail: detail_at(p.length()) };
        let f = self.frame.of(p);
        let n = lod_fbm(f, lod, streams.0[3]);
        let a = lod_threshold(&self.remap, lod, &|o: u32, x: f32, y: f32| lod_fbm(vec2f(x, y), Lod::whole(o), measure.0[3]));
        let density = self.remap.density(n, a, f);
        if !(density > 0.0) {
            return Shape { hit_m, height_m: self.height_m, ..Shape::default() };
        }
        let tau = SCUD_TAU * density;
        let cover = 1.0 - (-tau / d.y.max(MIN_MU)).exp();
        Shape { cover, alpha: cover * horizon_fade(d.y), tau, tau_sun: 0.0, hit_m, height_m: self.height_m }
    }
}

/// What a slab saw along a ray besides its combined shape: its own (without the scud), its scud's, and the rain
/// curtains' darkening in [0, 1].
#[derive(Clone, Copy, Debug, Default)]
pub struct SlabSeen {
    pub own: Shape,
    /// The depth its own light is seen through: a Stratus' deck's full depth there, so its thin fringe is lit as the
    /// deck is, a translucent grey rather than a bright rim; a Nimbostratus' own.
    pub lit_tau: f32,
    pub scud: Shape,
    pub curtain: f32,
}

/// A modern slab, ready to draw.
pub struct Slab {
    recipe: SlabRecipe,
    remap: CoverRemap,
    scud: Option<Scud>,
    height_m: f32,
    cell_m: f32,
    frame: WindFrame,
    streams: Streams,
    measure: Streams,
    /// mu toward the sun, for the shade it casts (plane_cover) and the light its scud gets.
    sun_mu: f32,
    /// What its threshold adds to the cover remap's, so its opacity's knee stands on the cover's quantile
    /// ([`knee_shift`]).
    knee_shift: f32,
}

impl Slab {
    /// The modern `layer` of `genus` at the effective cover `c`, or None for a type that is not a slab.
    pub fn build(layer: &CloudLayerParams, genus: CloudGenus, c: f32, map_seed: u32, downwind: Vec2f, sun_dir: Vec3f, height_m: f32) -> Option<Slab> {
        let stream = cloud_layer_seed(map_seed, layer.seed, genus);
        let drift_m = layer.drift_km.unwrap_or(0.0) * 1000.0;
        let (recipe, size_km, scud) = match genus {
            CloudGenus::Stratus => {
                let b = layer.stratus_params();
                let recipe = SlabRecipe::Stratus { tau: STRATUS_TAU_THIN + STRATUS_TAU_PER_DENSITY * b.density, mottling: b.mottling, aspect: 1.0 + STRATUS_STRETCH * b.streaks, ragged: b.ragged_edges };
                (recipe, b.size_km, None)
            }
            CloudGenus::Nimbostratus => {
                let b = layer.nimbostratus_params();
                let recipe = SlabRecipe::Nimbostratus { tau: NIMBO_TAU_LIGHT + NIMBO_TAU_PER_DARKNESS * b.darkness * b.darkness, unevenness: b.unevenness, ragged: b.ragged_base, curtains: b.rain_curtains };
                let scud_m = b.scud_height_m.min(b.base_m - SCUD_GAP_M);
                let scud = (b.scud > 0.0 && scud_m >= CLOUD_NIMBOSTRATUS_SCUD_HEIGHT_RANGE.0).then(|| {
                    let cell_m = b.size_km * 1000.0 * SCUD_CELL_SHARE;
                    let share = (b.scud * SCUD_SHARE * c).clamp(0.0, 1.0);
                    let remap = CoverRemap::new(quantile_set(fingerprint(genus, 1, &[])), share, SLAB_EDGE, hash_u32(stream ^ 0x5c0d_5c0d));
                    Scud { remap, height_m: scud_m, cell_m, frame: WindFrame::new(downwind, 2.0 * drift_m / cell_m) }
                });
                (recipe, b.size_km, scud)
            }
            _ => return None,
        };
        let cell_m = size_km * 1000.0;
        let remap = CoverRemap::new(quantile_set(fingerprint(genus, 0, &recipe.shape_fields())), c, SLAB_EDGE, stream);
        let sun = sun_dir.normalize();
        let sun_mu = if sun.is_finite() { sun.y.max(MIN_MU) } else { 1.0 };
        let knee_shift = knee_shift(c, recipe.half_opaque());
        Some(Slab { recipe, remap, scud, height_m, cell_m, frame: WindFrame::new(downwind, drift_m / cell_m), streams: Streams::of(stream), measure: Streams::of(MEASURE_SEED), sun_mu, knee_shift })
    }

    /// The height its scud is drawn at, when it has one.
    pub fn scud_height_m(&self) -> Option<f32> {
        self.scud.as_ref().map(|scud| scud.height_m)
    }

    /// The threshold at `lod`, its tables measured with MEASURE_SEED's streams, moved so that its opacity's knee stands
    /// on the cover's quantile.
    fn threshold(&self, lod: Lod) -> f32 {
        let measure = |octaves: u32, x: f32, y: f32| self.recipe.mask(vec2f(x, y), Lod::whole(octaves), &self.measure);
        lod_threshold(&self.remap, lod, &measure) + self.knee_shift
    }

    /// The rain curtains' bands round the horizon: value noise of the compass turn, seamless where the turn wraps.
    fn curtain_band(&self, d: Vec3f) -> f32 {
        let x = turn01(d.x, d.z) * CURTAIN_BANDS;
        let v = value2(x, 0.5, self.streams.0[4]);
        if x > CURTAIN_BANDS - 1.0 {
            v + (value2(x - CURTAIN_BANDS, 0.5, self.streams.0[4]) - v) * (x - (CURTAIN_BANDS - 1.0))
        } else {
            v
        }
    }

    /// The slab along the direction `d` (normalized here, so the stand-in is exactly the shape's), its scud and
    /// curtains folded in: the combined cover and alpha (the scud in front, lower), its depth tau x n at its own hit,
    /// and its parts for the light. Nothing at and below the horizon, nor along a direction that is not one.
    pub fn shape(&self, d: Vec3f) -> (Shape, SlabSeen) {
        let d = d.normalize();
        if !(d.y > 0.0) || !d.is_finite() {
            return (Shape::default(), SlabSeen::default());
        }
        let hit_m = plane_hit(d, self.height_m);
        let p = vec2f(hit_m.x / self.cell_m, hit_m.y / self.cell_m);
        let cycles = p.length() * self.recipe.mask_freq();
        let lod = Lod { fine: SLAB_OCTAVES, broad: SLAB_BROAD_OCTAVES, detail: detail_at(cycles) };
        let f = self.frame.of(p);
        let mut mask = self.remap.density(self.recipe.mask(f, lod, &self.streams), self.threshold(lod), f);
        // Far off a Stratus' patches merge into the share of its plane they cover, as a texel there holds many.
        if let SlabRecipe::Stratus { .. } = self.recipe {
            mask += (self.remap.cover() - mask) * smoothstep(MERGE_FROM, MERGE_TO, cycles);
        }
        let mut own = Shape { hit_m, height_m: self.height_m, ..Shape::default() };
        let mut curtain = 0.0f32;
        let mut lit_tau = 0.0f32;
        if mask > 0.0 {
            let deck = self.recipe.depth(f, p.length(), &self.streams);
            let (cover, tau) = self.recipe.opacity(deck, mask, d.y.max(MIN_MU));
            lit_tau = if let SlabRecipe::Stratus { .. } = self.recipe { deck } else { tau };
            own = Shape { cover, alpha: cover * horizon_fade(d.y), tau, tau_sun: 0.0, hit_m, height_m: self.height_m };
            if let SlabRecipe::Nimbostratus { curtains, .. } = self.recipe {
                if curtains > 0.0 && d.y < CURTAIN_TOP {
                    curtain = (curtains * mask * (1.0 - smoothstep(0.03, CURTAIN_TOP, d.y)) * smoothstep(0.35, 0.75, self.curtain_band(d))).clamp(0.0, 1.0);
                }
            }
        }
        let scud = self.scud.as_ref().map_or(Shape::default(), |scud| scud.shape(d, &self.streams, &self.measure));
        let cover = scud.cover + (1.0 - scud.cover) * own.cover;
        let alpha = scud.alpha + (1.0 - scud.alpha) * own.alpha;
        (Shape { cover, alpha, tau: own.tau, tau_sun: 0.0, hit_m, height_m: self.height_m }, SlabSeen { own, lit_tau, scud, curtain })
    }

    /// Its light along `d`, premultiplied by its alpha: the slab law (cloud_light.rs `radiance`) on its own shape, the
    /// dome reaching its base through its diffuse share as the sun's does, darkened where its curtains hang; with
    /// `scud`, its scud's alone instead (the stack composites the scud at its own height), by the same law, lit by the
    /// sun and the dome only through the slab's own depth, at half the light (a dark ragged sheet). A Nimbostratus'
    /// light turns a cool grey.
    pub fn radiance(&self, light: &EntryLight, seen: &SlabSeen, d: Vec3f, sun_dir: Vec3f, t_above: f32, scud: bool) -> Vec3f {
        // The law keeps half of the dome under any depth; a deep slab passes the dome as it passes the sun, by T_d, so
        // its base is grey under a high sun rather than glowing. A thin edge (T_d near 1) keeps the law's light.
        let passes = cloud_light::diffuse_transmittance(seen.lit_tau);
        let under = EntryLight { ambient: light.ambient * (passes / (0.5 + 0.5 * passes)), ..*light };
        let lit = Shape { tau: seen.lit_tau, ..seen.own };
        let seen_light = if !scud {
            cloud_light::radiance(&under, &lit, d, sun_dir, t_above) * (1.0 - CURTAIN_DARKEN * seen.curtain)
        } else if !(seen.scud.alpha > 0.0) {
            Vec3f::default()
        } else {
            let through = t_above * (-seen.own.tau / self.sun_mu).exp();
            // Under the deck the scud sees the dome only as the deck passes it.
            let below = EntryLight { ambient: light.ambient * passes, ..*light };
            cloud_light::radiance(&below, &seen.scud, d, sun_dir, through) * SCUD_LIGHT
        };
        match self.recipe {
            SlabRecipe::Nimbostratus { .. } => {
                let grey = NIMBO_TINT * (luminance(seen_light) / luminance(NIMBO_TINT));
                seen_light.mix(grey, NIMBO_COOL)
            }
            SlabRecipe::Stratus { .. } => {
                // The sun's place shows through as a brighter patch: the light the layer passes diffusely leans toward
                // the sun, the brighter the thinner the layer.
                let lean = (0.5 + 0.5 * d.dot(sun_dir)).max(0.0).powi(SUN_LEAN);
                let glow = SUN_GLOW * lean * sun_dir.y.max(0.0) * cloud_light::diffuse_transmittance(seen.lit_tau) * t_above;
                seen_light + light.sun * (glow * light.rows.brightness * seen.own.alpha)
            }
        }
    }

    /// What it hides of whatever is behind it along `d` (design §2.4): its combined unfaded cover along the unit
    /// direction, as a sheet's: exactly the cover the compositor's lookup along `d.normalize()` sees.
    pub fn cover_toward(&self, d: Vec3f) -> f32 {
        self.shape(d.normalize()).0.cover
    }

    /// What the light pass reads: its cover and its depth there (T_d, design §2.3).
    pub fn stand_in(&self, d: Vec3f) -> CoverSample {
        let (shape, _) = self.shape(d);
        CoverSample { cover: shape.cover, tau: shape.tau }
    }

    /// Its opacity toward the sun at a ground point of its plane (metres east and south), from `octaves` of its mask
    /// (3 released, 2 held), the broad table (D9) and its base tau: the shade it casts on a lower layer.
    pub fn plane_cover(&self, xz_m: Vec2f, octaves: u32) -> f32 {
        let f = self.frame.of(vec2f(xz_m.x / self.cell_m, xz_m.y / self.cell_m));
        let n = self.recipe.mask(f, Lod::whole(octaves.clamp(1, SLAB_OCTAVES)), &self.streams);
        let mask = self.remap.density(n, self.threshold(Lod::whole(SLAB_BROAD_OCTAVES)), f);
        if mask > 0.0 { self.recipe.opacity(self.recipe.base_tau(), mask, self.sun_mu).0 } else { 0.0 }
    }

    /// The mask's density at a point of its plane (cells east and south) at full detail (`cover_rises_from_zero`).
    pub fn density_at(&self, p_cells: Vec2f) -> f32 {
        let f = self.frame.of(p_cells);
        let lod = Lod::whole(SLAB_OCTAVES);
        self.remap.density(self.recipe.mask(f, lod, &self.streams), self.threshold(lod), f)
    }
}

/// What a slab of cover `c` whose mask is half opaque straight up at `half_opaque` (`SlabRecipe::half_opaque`) adds to
/// the cover remap's threshold q(1 - c) - w c, so that it is q(1 - c) - w k: the mask's noise at the cover's quantile
/// is k of the way up the edge, where the mask is `half_opaque` and the slab, seen straight up, is half opaque. Over the last TAIL_SHARE of cover k rises to 1, so at Cover 1 the
/// shift is 0 and the slab closes as the remap closes a sheet. Below KNEE_FROM k eases back to c, so at TAIL_SHARE the
/// shift is exactly 0: the tail gate's thinning assumes the remap's own share with any cloud there, and a thin slab's
/// knee, deep in its edge, would hold well over twice c of its plane under a faint fringe (design §3.4: the share with
/// any cloud stays within 2 c as the cover rises from 0). Below TAIL_SHARE the remap's c is held there, and so is this
/// one's. Worked out once per build.
fn knee_shift(c: f32, half_opaque: f32) -> f32 {
    let held = if c.is_nan() { TAIL_SHARE } else { c.clamp(TAIL_SHARE, 1.0) };
    // The edge's t where smoothstep(0, 1, t) is the mask's knee.
    let knee = if half_opaque < 1.0 { 0.5 - ((1.0 - 2.0 * half_opaque).asin() / 3.0).sin() } else { 1.0 };
    // Written so that k is exactly 1 at Cover 1, and the shift exactly 0.
    let k = 1.0 - (1.0 - knee) * (1.0 - smoothstep(1.0 - TAIL_SHARE, 1.0, held));
    // Written so that k is exactly the knee's from KNEE_FROM on, and exactly c at TAIL_SHARE.
    let s = smoothstep(TAIL_SHARE, KNEE_FROM, held);
    let k = k * s + held * (1.0 - s);
    SLAB_EDGE * (held - k)
}

/// The compass turn of a horizontal direction (x, z) as a pseudo-angle in [0, 1): the diamond angle, monotone and
/// continuous round the circle, with no trigonometry.
fn turn01(x: f32, z: f32) -> f32 {
    let s = x.abs() + z.abs();
    if !(s > 0.0) {
        return 0.0;
    }
    let (x, z) = (x / s, z / s);
    let quarter = if z >= 0.0 {
        if x >= 0.0 { z } else { 1.0 - x }
    } else if x < 0.0 {
        2.0 - z
    } else {
        3.0 + x
    };
    (quarter * 0.25).clamp(0.0, 0.999_999)
}

/// Local copy (as in clouds.rs), so no glob can make the name ambiguous.
fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_classic::{downwind, stand_height_m};
    use crate::dir_from_az_el;

    /// Each slab at its defaults, from the tail's cover to closed, is nothing at and below the horizon and along
    /// directions that are not ones; above it every number is finite, its covers in [0, 1] and never under its alphas,
    /// the scud's and the slab's own included, the stand-in its cover and depth; a closed default Nimbostratus hides the
    /// sky everywhere above 5 degrees. A scud stays under its slab: at Scud height, at most SCUD_GAP_M under the base, and
    /// none on a base too low for the lowest Scud height (the default Nimbostratus keeps its 500 m scud).
    #[test]
    fn the_slabs_are_finite_and_empty_below_the_horizon() {
        let sun = dir_from_az_el(200.0, 30.0);
        for genus in [CloudGenus::Stratus, CloudGenus::Nimbostratus] {
            let layer = CloudLayerParams::new(genus, &[], 1);
            for c in [0.0005f32, 0.05, 0.4, 1.0] {
                let slab = Slab::build(&layer, genus, c, 1, downwind(30.0), sun, stand_height_m(&layer, genus)).expect("a slab");
                for el in [0.0f32, -0.5, -10.0, -90.0] {
                    for az in (0..360).step_by(30) {
                        let d = dir_from_az_el(az as f32, el);
                        let (s, parts) = slab.shape(d);
                        assert_eq!((s.cover, s.alpha, s.tau, parts.curtain), (0.0, 0.0, 0.0, 0.0), "{genus:?} at {c}: ({az}, {el})");
                        assert_eq!(slab.cover_toward(d), 0.0);
                    }
                }
                for d in [Vec3f::default(), vec3f(f32::NAN, 1.0, 0.0), vec3f(f32::INFINITY, 1.0, 0.0)] {
                    assert_eq!(slab.cover_toward(d), 0.0, "{genus:?}: {d:?}");
                }
                let mut most = 0.0f32;
                for i in 0..720 {
                    let d = dir_from_az_el(i as f32 * 7.7, 0.2 + (i % 90) as f32);
                    let (s, parts) = slab.shape(d);
                    for shape in [s, parts.own, parts.scud] {
                        assert!(shape.cover.is_finite() && shape.alpha.is_finite() && shape.tau.is_finite() && shape.tau >= 0.0, "{genus:?} at {c}: {shape:?}");
                        assert!((0.0..=1.0).contains(&shape.cover) && shape.alpha <= shape.cover && shape.alpha >= 0.0, "{genus:?} at {c}: {shape:?}");
                    }
                    assert!((0.0..=1.0).contains(&parts.curtain));
                    let stand_in = slab.stand_in(d);
                    assert_eq!((stand_in.cover, stand_in.tau), (s.cover, s.tau));
                    let p = slab.plane_cover(vec2f(i as f32 * 731.0 - 50_000.0, (i % 37) as f32 * 1777.0 - 30_000.0), 3);
                    assert!((0.0..=1.0).contains(&p), "{genus:?}: plane cover {p}");
                    most = most.max(s.cover);
                }
                if c >= 0.05 {
                    assert!(most > 0.0, "{genus:?} at {c}: some cloud shows");
                }
            }
        }
        for (base, scud_m, drawn) in [(1000.0, 500.0, Some(500.0)), (300.0, 500.0, Some(250.0)), (1000.0, 1500.0, Some(950.0)), (200.0, 150.0, Some(150.0)), (199.0, 150.0, None), (100.0, 500.0, None)] {
            let mut layer = CloudLayerParams::new(CloudGenus::Nimbostratus, &[], 1);
            let b = layer.nimbostratus.get_or_insert_with(Default::default);
            (b.base_m, b.scud_height_m) = (base, scud_m);
            let slab = Slab::build(&layer, CloudGenus::Nimbostratus, 0.5, 1, downwind(30.0), sun, stand_height_m(&layer, CloudGenus::Nimbostratus)).unwrap();
            assert!(slab.scud_height_m() == drawn && slab.scud_height_m().map_or(true, |h| h < base), "a {scud_m} m scud under a {base} m base: {:?}", slab.scud_height_m());
        }
        let closed = CloudLayerParams::new(CloudGenus::Nimbostratus, &[], 1);
        let slab = Slab::build(&closed, CloudGenus::Nimbostratus, 1.0, 1, downwind(30.0), sun, stand_height_m(&closed, CloudGenus::Nimbostratus)).unwrap();
        for i in 0..360 {
            let d = dir_from_az_el(i as f32, 5.0 + (i % 85) as f32);
            assert!(slab.cover_toward(d) > 0.999, "a closed Nimbostratus at {d:?}: {}", slab.cover_toward(d));
        }
    }
}
