//! The floor (plan hdri-8): the map's lower hemisphere in Sky mode as a ground of one kind (`FloorParams`), seen
//! from `eye_m` over a level ground in the map's own frame.
//!
//! This file holds the floor's far model (§2, §3): what each kind looks like beyond its near relief, the whole floor
//! when no relief is rendered. It gives, along a direction below the horizon, the ground's albedo (the kind's mean
//! material times a large-scale "distant map" variation, every octave band-limited to the lookup's texel), its water
//! and ice (the Lake), the water's ripple normal, and how much of its blend into the horizon haze a far valley keeps.
//! The map lights it with its own light exactly as it lit its flat ground (`Env::sky_layers`, `Env::base_layer`):
//! a floor of one plain albedo is the old ground of that colour.
//!
//! The kinds' material constants are one table, [`FloorModel::recipe`], which the app copies into the GPU's ask, so
//! the near relief and the far model read the same colours (§4.9).

use makepad_draw::*;

use makepad_render_material::ibl;

use super::noise::hash01;
use super::params::{FloorKind, FloorParams, FloorPattern, PlazaJoints, RoadBeside, FLOOR_FAR_SHORE_RANGE};
use super::rain_water::RainOnWater;
use super::weather::SNOW_ALBEDO;
use std::sync::Arc;

/// One kind's material constants (linear albedo, 0..1), the one table the GPU's near relief and the far model read
/// (plan hdri-8 §4.9). What each colour is depends on the kind:
///
/// | Kind | `albedo` | `alt` | `under` |
/// |---|---|---|---|
/// | Grass | healthy blades | golden blades (Dryness 1) | bare soil |
/// | Sand | sand | crests | troughs |
/// | Snow | snow | drift crests | old snow in the hollows |
/// | Rock | stone | gravel | the soil in the cracks |
/// | Road | asphalt | paint | the verge (gravel and short grass) |
/// | Plaza | tiles | the dirt in the joints (a model's own: the joints' fill, `PLAZA_SAND`, `PLAZA_MORTAR`) | moss |
/// | Mountain | the summit's rock | the valley's dry grass and scrub | the valley's rock |
/// | Lake | the shore's pebbles and sand | the deep water's body (what its depths send back up, dark) | the far bank's soil (half of it; Grass the rest) |
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FloorRecipe {
    pub albedo: [f32; 3],
    pub alt: [f32; 3],
    pub under: [f32; 3],
    /// The main surface's roughness, 0..1.
    pub roughness: f32,
}

/// A road lane's width, metres (§3.5).
pub const LANE_WIDTH_M: f32 = 3.5;
/// The Plaza's joints filled with sand (`PlazaJoints::Sand`): warm and brown, darker than the tiles (the jointing sand
/// of the survey's running-bond paving, r/g 1.3, b/g 0.7), nearly flush.
pub const PLAZA_SAND: [f32; 3] = [0.19, 0.145, 0.10];
/// The Plaza's joints filled with mortar (`PlazaJoints::Mortar`): a light warm grey, half as light again as the tiles
/// (the fan setts' mortar, 1.8 times their red setts' luminance, r/g 1.1 to 1.2, b/g 0.85), nearly flush.
pub const PLAZA_MORTAR: [f32; 3] = [0.40, 0.37, 0.32];
/// The verge's width either side of a road, metres; a Grass field beyond it.
pub const VERGE_M: f32 = 3.0;
/// The Lake's "in front": the map azimuth (degrees, a light's azimuth) the water lies toward; the shore is behind,
/// about azimuth 180 (§3.8). The map's Rotation turns it with everything else.
pub const LAKE_FRONT_AZ_DEG: f32 = 0.0;
/// The Lake's beach behind the viewer, metres; its far bank beyond.
pub const LAKE_BEACH_M: f32 = 20.0;
/// Ice's reflectance head-on (Schlick's F0, §3.8).
pub const ICE_F0: f32 = 0.018;
/// Ice's own albedo under its mirror: clear ice over dark water, a dark teal body with its crack sheets and bubbles
/// (the near ice's mean, `makepad-floor`'s Lake shader, so the near ice and the far model meet).
pub const ICE_ALBEDO: [f32; 3] = [0.09, 0.18, 0.21];
/// The share of a frozen lake snow lies on in patches (the far model's mean of the near ice's mask).
pub const ICE_SNOW_SHARE: f32 = 0.1;
/// Ice's roughness: the share of its mirror blurred into the sky's mean.
pub const ICE_ROUGHNESS: f32 = 0.3;
/// The slush in a snowed road's wheel tracks (Q5).
pub const SLUSH_ALBEDO: [f32; 3] = [0.30, 0.30, 0.31];
/// A Mountain's summit radius runs from this to twice it, metres, by the seed (§3.7).
pub const SUMMIT_MIN_M: f32 = 10.0;
/// The distance over which a Mountain's valley fades into the horizon haze (its aerial perspective), metres.
pub const MOUNTAIN_HAZE_M: f32 = 25_000.0;
/// The farthest a planes' texel's distance says, metres: the planes keep it as a half float and the GPU's march holds
/// it to 65 km (`makepad-floor`), so a texel at about this shows ground at least that far (`FloorHere::dist`).
const PLANES_DIST_MAX_M: f32 = 64_000.0;

/// The beach a floor lays on its own side of the skyline's sea (plan hdri-8 §3.10, D2), metres of ground inward from
/// its shoreline, at the floor's own eye (matched to the strip by angle, Q1).
pub const SEA_BEACH_M: f32 = 4.0;
/// The ripples of the skyline's sea where a floor carries it on (its kind not a Lake, whose own Ripples it takes): the
/// Lake's default.
pub const SEA_RIPPLES: f32 = 0.3;

/// The skyline's sea as the floor carries it on below the strip (plan hdri-8 §3.10, the owner's D2), in the map's own
/// frame: `n` columns round the turn, column i at equirect u = (i + 0.5) / n, each the span of depressions (rad) the
/// strip's sea covers there, [far, near] (makepad_skyline's `sea_spans`, the strip's turn applied by the app): its near
/// edge the shore, its far edge 0 over open sea; [0, 0] where there is none. Read between columns linearly, wrapping:
/// the far model and the GPU's planes read it alike (`SeaEdge::texels`), so the floor's water is the strip's sea,
/// matched by angle (Q1).
#[derive(Clone, Debug, PartialEq)]
pub struct SeaEdge {
    spans: Vec<[f32; 2]>,
}

impl SeaEdge {
    /// The edge from its columns, made safe: a NaN is 0, and every edge is held to 0..pi/2.
    pub fn new(mut spans: Vec<[f32; 2]>) -> SeaEdge {
        for s in spans.iter_mut() {
            for e in s.iter_mut() {
                *e = if e.is_nan() { 0.0 } else { e.clamp(0.0, std::f32::consts::FRAC_PI_2) };
            }
        }
        SeaEdge { spans }
    }

    /// Its columns.
    pub fn columns(&self) -> usize {
        self.spans.len()
    }

    /// The span [far, near] at equirect u (0..1), linear between the two nearest columns, wrapping.
    pub fn at(&self, u: f32) -> [f32; 2] {
        let n = self.spans.len();
        if n == 0 {
            return [0.0; 2];
        }
        let x = u.rem_euclid(1.0) * n as f32 - 0.5;
        let x0 = x.floor();
        let t = x - x0;
        let (a, b) = (self.spans[(x0 as i64).rem_euclid(n as i64) as usize], self.spans[(x0 as i64 + 1).rem_euclid(n as i64) as usize]);
        [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
    }

    /// The columns as the GPU's ask takes them (`makepad_floor::FloorAsk::sea_edge`): far and near of each, in order.
    pub fn texels(&self) -> Vec<f32> {
        self.spans.iter().flatten().copied().collect()
    }
}

/// The distant map (§3.9): the large-scale colour variation over the ground, three octaves of value noise. Their
/// wavelengths (metres; x VALLEY_SCALE over a Mountain's valley), their brightness amplitudes, and the share of the
/// kind's alternate colour the largest drifts the hue by at most.
const DISTANT_WAVELENGTHS_M: [f32; 3] = [80.0, 40.0, 20.0];
const DISTANT_AMPLITUDES: [f32; 3] = [0.10, 0.06, 0.04];
const DISTANT_HUE: f32 = 0.1;
const VALLEY_SCALE: f32 = 25.0;
/// Band limit: an octave of wavelength l is whole from BAND_FULL ground footprints up and gone at BAND_GONE (the
/// plan's floor is two; the noise's quintic cells carry harmonics well above their wavelength).
const BAND_GONE: f32 = 4.0;
const BAND_FULL: f32 = 16.0;
/// The Lake's ripples: wind waves from a 3 m swell down to 6 cm wavelets, seeded around one wind direction (the
/// shorter the wider their spread), each wave's crests bent by a slow warp along them (Day at the Lake's domain-warped
/// waves, our own form). Their wavelengths (metres), each wave's slope at Ripples 1 (a wind spectrum's slope is about
/// even per octave), how far a crest is bent (radians of phase) and how fast along it (a share of its wavenumber).
const RIPPLE_WAVELENGTHS_M: [f32; RIPPLE_WAVES] = [3.0, 2.0, 1.3, 0.85, 0.55, 0.36, 0.24, 0.16, 0.105, 0.07];
const RIPPLE_WAVES: usize = 10;
const RIPPLE_SLOPE: f32 = 0.12;
const RIPPLE_BEND: f32 = 1.8;
const RIPPLE_BEND_RATE: f32 = 0.45;
/// A ripple (a pure wave, no harmonics but its bend's) is whole from RIPPLE_FULL footprints up and gone at
/// RIPPLE_GONE: laxer than the noise octaves' BAND_FULL and BAND_GONE.
const RIPPLE_GONE: f32 = 2.5;
const RIPPLE_FULL: f32 = 7.0;
/// The Lake's gusts (§3.8; the dunes lane, 2026-11-09): wind over water is never even. Its gusts raise the ripples in
/// lanes drawn out along it between calmer ones, and in the lee of a shore it blows off, the water lies glassy for
/// its first tens of metres (Physical Open Waters' capillary mask, written our own way). One factor on the ripples'
/// slopes at each ground point (`FloorModel::gust`) sets both how strong the ripples the mirror shows are and how
/// rough the mirror is: the slopes the texel leaves unresolved, which the mirror turns by (`visible_tilt`). The sun's
/// glitter keeps the lake's mean ripples, so its light holds across bake widths. The lanes' noise cells along the wind and across it (two octaves, the second at 0.4 of the
/// first with 0.3 of its weight), each whole from GUST_FULL of its cells a footprint and gone at GUST_GONE; the
/// factor 0.35..1.45 times GUST_SCALE, which makes its mean square 1 over open water (the lake's mean sheen and
/// glitter hold); the lee: GUST_LEE of the gusts' factor within GUST_LEE_M.0 of the shore up-wind, all of it from
/// GUST_LEE_M.1 (a low bank's lee: no trees or walls stand on our shores).
const GUST_CELLS_M: [f32; 2] = [160.0, 50.0];
const GUST_GONE: f32 = 1.5;
const GUST_FULL: f32 = 4.0;
const GUST_SCALE: f32 = 1.0327;
const GUST_LEE: f32 = 0.15;
const GUST_LEE_M: (f32, f32) = (5.0, 60.0);
/// How far outside the shore's sector edge a point inside it is taken to lie for its lee (`lee`), metres: on the water
/// side, so the edge's own crossing tells which way the wind blows across it.
const LEE_EDGE_OUT_M: f32 = 1.0e-3;
/// The facets one texel of rippled water mirrors by (`mirror_lobe`): the visible slopes along the look's plane from
/// where they turn their backs to the eye up to MIRROR_LOBE_U deviations, in steps of MIRROR_LOBE_DU deviations up to
/// MIRROR_LOBE_DU_MOST as the table they are read from is coarser (at most 2 MIRROR_LOBE_U / MIRROR_LOBE_DU of them;
/// past 4 deviations a Gaussian's weight is under 1e-3 of its peak).
const MIRROR_LOBE_U: f32 = 4.0;
const MIRROR_LOBE_DU: f32 = 0.25;
const MIRROR_LOBE_DU_MOST: f32 = 1.0;
const MIRROR_LOBE_NODES: usize = 32;

/// What the far model gives along one direction below the horizon.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FloorFar {
    /// The ground's albedo there (tint and the weather's snow in): under water the lake bed, under ice the ice and
    /// its snow.
    pub albedo: Vec3f,
    /// The share of the texel that is open water, 0..1.
    pub water: f32,
    /// The share that is ice (a frozen lake), 0..1.
    pub ice: f32,
    /// The water's normal (its ripples), unit, facing up; straight up where it is flat. On the Lake the mean of the
    /// facets the eye sees (`visible_tilt`), which the mirror's one exact lookup takes.
    pub normal: Vec3f,
    /// How much of the atmosphere's blend into the horizon haze the ground keeps (`Atmosphere::sky_on_ground`): 1,
    /// less over a far valley.
    pub keep: f32,
    /// The distant map's brightness factor there (1 + its octaves; 1 without it): what the near relief's albedo is
    /// multiplied by too, so the variation runs on across the planes' edge (plan hdri-8 T2).
    pub shade: f32,
    /// The ripples' normal as the texel resolves them, also where `normal` is left level (a held bake): the sun's
    /// glitter is centred on its slope (`glitter`, `Env::floor_waters`), so it sparkles with the ripples the texel shows.
    pub glint: Vec3f,
    /// The variance (per axis) of the ripple slopes `glint` leaves out (0 on flat water and off the water): the waves
    /// too fine for the texel and the kept ones' turn within it. The glitter's lobe spreads by it, so it never sparkles
    /// from texel to texel and its light holds across the bake's widths (§3.8).
    pub slopes: f32,
    /// The variance (per axis) of all the ripples' slopes (0 on flat water and off the water): the glitter's masking.
    pub slopes_all: f32,
    /// The variance (per axis) of the slopes the mirror leaves unresolved, as the Lake's gusts scale them: the facets
    /// the texel's water mirrors the sky by spread round `normal`, their visible mean (`mirror_lobe`). 0 where the
    /// mirror is one direction: flat water, the skyline's sea, off the water.
    pub rough: f32,
    /// How far the ground it shows lies along the direction, metres: the eye over the sine of the depression, the
    /// Mountain's valley its drop and the eye below (f32::MAX at or above level). The air between is what a viewer
    /// height's sky sees it through (`Atmosphere::sky_over_at`).
    pub dist: f32,
}

/// The floor's far model of one map (§2's far zone; the whole floor when no relief is attached).
#[derive(Clone, Debug)]
pub struct FloorModel {
    kind: FloorKind,
    seed: u32,
    eye: f32,
    reach: f32,
    numbers: [f32; 8],
    recipe: FloorRecipe,
    /// The weather's snow covers the floor (`effects(weather).snow`); a Lake is then frozen.
    snow: bool,
    /// The distant map's variation is drawn (off only in the flat floor's test).
    vary: bool,
    /// The kind's main ground's mean albedo, tint and snow in (a Grass field's looking straight down; `main_at`).
    main: Vec3f,
    /// How the main ground's albedo grows with the sine of the depression (a Grass field's soil between the blades,
    /// seen looking down; 0 for the other kinds): `main_at(s)` is `main + main_slope x s`.
    main_slope: Vec3f,
    /// The hue the distant map drifts toward.
    drift: Vec3f,
    /// Road: the verge and the field beyond it; Lake: the beach and the far bank; Mountain: the valley (`second`).
    second: Vec3f,
    third: Vec3f,
    /// Road: its centre line on the ground (straight or bent) and its half width, metres.
    road: RoadPath,
    half: f32,
    haze: f32,
    /// Road: the verge's width beyond its edge, metres (`FloorParams::verge_m`: VERGE_M beside grass, 0 beside the rest).
    verge: f32,
    /// Mountain: the summit's radius, metres.
    summit: f32,
    /// The Grass field beside a road and on a lake's far bank: Grass at its default dryness, under this floor's tint,
    /// without the weather's snow (`field_albedo`).
    field: Vec3f,
    /// Lake, and the skyline's sea on any kind: the ripples' waves, each its direction on the ground (x, z), its
    /// wavenumber (radians a metre), its phase and its bend's phase, seeded once.
    waves: [[f32; 5]; RIPPLE_WAVES],
    /// The skyline is drawn over this floor (plan hdri-8 §3.10): a Lake's water runs on under the strip to the horizon
    /// (Far shore applies only without the skyline, Q3).
    skyline: bool,
    /// The skyline's sea the floor carries on below the strip (D2): None without a skyline, a sea or for a Mountain,
    /// whose valley takes everything below the horizon (Q4).
    sea: Option<Arc<SeaEdge>>,
    /// Lake: the frame its gusts are worked out in (`gust`, `lee`), once.
    gusts: Option<GustFrame>,
    /// The rain falling on its water (rain_water.rs; the weather lane, 2026-11-09): the Lake's and the skyline's sea's
    /// mirror takes the rain's rings and sheen (`with_rain`). None without rain or under snow.
    rain: Option<RainOnWater>,
    /// The damp film's slopes (variance per axis) on the main, second and third grounds (`damp_films`), which
    /// `damp_slopes_at` lays as the albedo's shares are laid.
    films: [f32; 3],
}

/// The Lake's gusts' frame (`FloorModel::gusts`): the ripples' seeded wind, the direction it blows toward (x, z, as
/// `ripple_waves` turns them), and the shore's sector's two edges (None at Shore 0), each its direction out from the
/// eye's foot and its normal into the land (x, z, x, z).
#[derive(Clone, Copy, Debug)]
struct GustFrame {
    wind: [f32; 2],
    edges: Option<[[f32; 4]; 2]>,
}

impl GustFrame {
    /// The frame of seed `seed` and Shore `shore` (0..1: the sector Shore x 90 degrees either side of straight behind).
    fn new(seed: u32, shore: f32) -> GustFrame {
        let wind = std::f32::consts::TAU * hash01(seed as i32, 0, 0, 0x7199);
        let sector = shore * std::f32::consts::FRAC_PI_2;
        let behind = LAKE_FRONT_AZ_DEG.to_radians() + std::f32::consts::PI;
        // (an edge at azimuth e runs out along (sin e, -cos e); the land lies toward the rising azimuth from the first,
        // the falling one from the second)
        let edge = |e: f32, side: f32| [e.sin(), -e.cos(), side * e.cos(), side * e.sin()];
        GustFrame { wind: [wind.cos(), wind.sin()], edges: (sector > 0.0).then(|| [edge(behind - sector, 1.0), edge(behind + sector, -1.0)]) }
    }
}

/// The mirror's normal over rippled water at ground point (x, z) seen along a look of depression sine `s`: the resolved
/// ripples' `normal` turned toward the eye by the mean slope of the facets the eye sees among the slopes the texel
/// leaves unresolved (variance `left` per axis). At a grazing look rough water hides its facets turned away, so what it
/// mirrors lies higher in the sky, and glassy water mirrors the horizon: Smith's visible-normal mean for Gaussian
/// slopes of deviation sigma, sigma Phi(t) / (t Phi(t) + phi(t)) with t the look's slope over sigma (1.25 sigma at
/// the horizon, nothing looking down). `normal` itself where nothing is unresolved.
pub(crate) fn visible_tilt(normal: Vec3f, left: f32, x: f32, z: f32, s: f32) -> Vec3f {
    let r = (x * x + z * z).sqrt();
    if !(left > 0.0) || !(r > 0.0) || !(normal.y > 0.0) {
        return normal;
    }
    let sigma = left.sqrt();
    let t = s / (1.0 - s * s).max(1.0e-12).sqrt() / sigma;
    let tilt = sigma * visible_mean(t);
    // (toward the eye: the ground point's direction from it, reversed)
    vec3f(normal.x / normal.y - tilt * x / r, 1.0, normal.z / normal.y - tilt * z / r).normalize()
}

/// The mean slope toward the eye of the facets it sees, in deviations of Gaussian slopes, for a look whose slope is
/// `t` deviations: Phi(t) / (t Phi(t) + phi(t)) (`visible_tilt`; 1.25 at the horizon).
fn visible_mean(t: f32) -> f32 {
    let (cdf, pdf) = normal_cdf_pdf(t);
    cdf / (t * cdf + pdf)
}

/// The standard normal distribution's CDF (by its tanh form, within 3e-4) and density at `t`.
fn normal_cdf_pdf(t: f32) -> (f32, f32) {
    (0.5 * (1.0 + (0.797_884_6 * (t + 0.044_715 * t * t * t)).tanh()), (-0.5 * t * t).exp() * 0.398_942_3)
}

/// How far the facets `mirror_lobe` spreads the mirror by turn its elevation, along the look `d` (unit, below the
/// horizon) over water leaving `rough` per axis unresolved: the standard deviation, radians, of twice their visible
/// slopes along the look (of the density (t + u) phi(u) from u = -t: its mean Phi / Z, its mean square
/// (t Phi + 2 phi) / Z, Z = t Phi + phi). 0 without unresolved slopes.
pub(crate) fn mirror_spread(d: Vec3f, rough: f32) -> f32 {
    let s = (-d.y).min(1.0);
    if !(rough > 0.0) || !(s > 0.0) {
        return 0.0;
    }
    let sigma = rough.sqrt();
    let t = s / (1.0 - s * s).max(1.0e-12).sqrt() / sigma;
    let (cdf, pdf) = normal_cdf_pdf(t);
    let z = t * cdf + pdf;
    let mean = cdf / z;
    2.0 * sigma * ((t * cdf + 2.0 * pdf) / z - mean * mean).max(0.0).sqrt()
}

/// What water of normal `n` mirrors along the look `d` (unit): `d` reflected about it, held above the horizon (a facet
/// that would mirror the ground mirrors the horizon's sky instead).
pub(crate) fn mirror_of(d: Vec3f, n: Vec3f) -> Vec3f {
    let m = d - n * (2.0 * d.dot(n));
    if m.y > 0.0 { m } else { vec3f(m.x, 1.0e-4, m.z).normalize() }
}

/// The facets one texel of rippled water mirrors the sky by (plan hdri-8 §3.8; the dunes lane's fixes, 2026-11-09):
/// `node(m, c, w)` for each, its mirror direction m (`mirror_of`), the cosine c of the look against it (its Fresnel's)
/// and its weight w (the weights sum to 1). The texel's water mirrors the sky through every facet it sees among the
/// slopes it leaves unresolved, `rough` per axis round their visible mean `normal` (`visible_tilt`, along the look
/// `d`, unit, below the horizon): Smith's visible slopes along the look's plane, density (t + u) phi(u) from u = -t
/// (the facets turned away from the eye) in deviations, t the look's slope over sigma, on a grid of MIRROR_LOBE_DU
/// deviations, or of half the mirror's elevation `step` (radians) where that is coarser (the spacing of the table the
/// caller reads the facets' sky from: a finer grid finds nothing more in it), at most MIRROR_LOBE_DU_MOST.
/// Across the look's plane a facet turns the mirror by its slope times twice the look's own, which at the grazing
/// looks rough water spreads its mirror at is too little to spread. Where the mirror's lookup is one direction (no
/// unresolved slopes, a level look) one node: `normal` itself, weight 1.
/// At the default Ripples the far water's facets mirror the sky from the horizon to some 20 degrees up, about 9 on
/// average: a lookup along their mean alone loses the low sky's light (a skyline's city, the haze band, a sunset's
/// glow), which these spread back over the water.
pub(crate) fn mirror_lobe(d: Vec3f, normal: Vec3f, rough: f32, step: f32, node: &mut dyn FnMut(Vec3f, f32, f32)) {
    let s = (-d.y).min(1.0);
    let h = (d.x * d.x + d.z * d.z).sqrt();
    if !(rough > 0.0) || !(h > 0.0) || !(s > 0.0) || !(normal.y > 0.0) {
        node(mirror_of(d, normal), (-d.dot(normal)).clamp(0.0, 1.0), 1.0);
        return;
    }
    let sigma = rough.sqrt();
    let t = s / (1.0 - s * s).max(1.0e-12).sqrt() / sigma;
    let mean = visible_mean(t);
    // the grid's midpoints from where the facets turn their backs to the eye, and their weights (a facet's slope u
    // deviations turns the mirror's elevation by about 2 sigma u)
    let du = (0.5 * step / sigma).clamp(MIRROR_LOBE_DU, MIRROR_LOBE_DU_MOST);
    let from = (-t).max(-MIRROR_LOBE_U);
    let count = (((MIRROR_LOBE_U - from) / du).ceil() as usize).clamp(1, MIRROR_LOBE_NODES);
    let mut weights = [0.0f32; MIRROR_LOBE_NODES];
    let mut total = 0.0f32;
    for (i, w) in weights.iter_mut().enumerate().take(count) {
        let u = from + (i as f32 + 0.5) * du;
        *w = (t + u).max(0.0) * (-0.5 * u * u).exp();
        total += *w;
    }
    if !(total > 0.0) {
        node(mirror_of(d, normal), (-d.dot(normal)).clamp(0.0, 1.0), 1.0);
        return;
    }
    // each facet's slope: the mean's, turned along the look by its own visible slope less the mean (away from the eye
    // is along the look's horizontal direction)
    let (hx, hz) = (d.x / h, d.z / h);
    let (px, pz) = (normal.x / normal.y, normal.z / normal.y);
    for (i, &w) in weights.iter().enumerate().take(count) {
        let u = from + (i as f32 + 0.5) * du;
        let k = sigma * (mean - u);
        let n = vec3f(px + k * hx, 1.0, pz + k * hz).normalize();
        node(mirror_of(d, n), (-d.dot(n)).clamp(0.0, 1.0), w / total);
    }
}

/// The ripples' waves of seed `seed` (`FloorModel::waves`): around one seeded wind, the shorter the wider their spread.
fn ripple_waves(seed: u32) -> [[f32; 5]; RIPPLE_WAVES] {
    let wind = std::f32::consts::TAU * hash01(seed as i32, 0, 0, 0x7199);
    std::array::from_fn(|k| {
        let spread = 0.7 + 0.8 * k as f32 / (RIPPLE_WAVES - 1) as f32;
        let angle = wind + spread * (2.0 * hash01(seed as i32, k as i32, 1, 0x7199) - 1.0);
        let phase = std::f32::consts::TAU * hash01(seed as i32, k as i32, 2, 0x7199);
        let bend = std::f32::consts::TAU * hash01(seed as i32, k as i32, 3, 0x7199);
        [angle.cos(), angle.sin(), std::f32::consts::TAU / RIPPLE_WAVELENGTHS_M[k], phase, bend]
    })
}

use crate::patterns::smooth;

/// An edge at `x` = 0 softened over `width` (a footprint): 0 well below, 1 well above; a hard step at width 0.
fn soft_step(x: f32, width: f32) -> f32 {
    if width > 0.0 {
        smooth(-0.5 * width, 0.5 * width, x)
    } else if x > 0.0 {
        1.0
    } else if x < 0.0 {
        0.0
    } else {
        0.5
    }
}

/// How much of an octave a wavelength `ratio` ground footprints long keeps (§3.9): smooth(BAND_GONE, BAND_FULL, ratio).
fn band_ratio(ratio: f32) -> f32 {
    let t = ((ratio - BAND_GONE) * (1.0 / (BAND_FULL - BAND_GONE))).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Value noise in 0..1 on a unit lattice (mean 0.5), one integer hash a corner and a cubic fade: the distant map's
/// octaves, cheap enough for every texel of a 4K bake's lower half.
fn cell_noise(x: f32, z: f32, seed: u32) -> f32 {
    let (fx, fz) = (x.floor(), z.floor());
    let (tx, tz) = (x - fx, z - fz);
    let (ix, iz) = (fx as i32 as u32, fz as i32 as u32);
    let corner = |i: u32, j: u32| {
        let mut h = i.wrapping_mul(0x8da6_b343) ^ j.wrapping_mul(0xd816_3841) ^ seed;
        h ^= h >> 15;
        h = h.wrapping_mul(0x2c1b_3c6d);
        h ^= h >> 12;
        h = h.wrapping_mul(0x297a_2d39);
        h ^= h >> 15;
        (h >> 8) as f32 * (1.0 / 16_777_216.0)
    };
    let (sx, sz) = (tx * tx * (3.0 - 2.0 * tx), tz * tz * (3.0 - 2.0 * tz));
    let a = corner(ix, iz) + (corner(ix.wrapping_add(1), iz) - corner(ix, iz)) * sx;
    let b = corner(ix, iz.wrapping_add(1)) + (corner(ix.wrapping_add(1), iz.wrapping_add(1)) - corner(ix, iz.wrapping_add(1))) * sx;
    a + (b - a) * sz
}

fn v3(a: [f32; 3]) -> Vec3f {
    vec3f(a[0], a[1], a[2])
}

fn arr3(v: Vec3f) -> [f32; 3] {
    [v.x, v.y, v.z]
}

impl FloorModel {
    /// The kinds' material constants (the table in [`FloorRecipe`]'s notes).
    pub fn recipe(kind: FloorKind) -> FloorRecipe {
        let r = |albedo, alt, under, roughness| FloorRecipe { albedo, alt, under, roughness };
        match kind {
            FloorKind::Grass => r([0.11, 0.19, 0.05], [0.35, 0.28, 0.12], [0.20, 0.15, 0.10], 0.8),
            FloorKind::Sand => r([0.55, 0.35, 0.16], [0.60, 0.39, 0.185], [0.49, 0.305, 0.135], 0.9),
            FloorKind::Snow => r([0.85, 0.85, 0.85], [0.88, 0.88, 0.90], [0.76, 0.77, 0.80], 0.7),
            FloorKind::Rock => r([0.23, 0.205, 0.17], [0.30, 0.27, 0.215], [0.15, 0.12, 0.09], 0.85),
            FloorKind::Road => r([0.105, 0.099, 0.092], [0.65, 0.65, 0.62], [0.18, 0.205, 0.113], 0.75),
            FloorKind::Plaza => r([0.25, 0.235, 0.215], [0.11, 0.10, 0.085], [0.06, 0.08, 0.03], 0.7),
            FloorKind::Mountain => r([0.24, 0.185, 0.14], [0.12, 0.095, 0.055], [0.22, 0.17, 0.13], 0.85),
            FloorKind::Lake => r([0.30, 0.27, 0.22], [0.014, 0.020, 0.020], [0.20, 0.15, 0.10], 0.8),
        }
    }

    /// The far model of `floor` (read as clamp() holds it) under a weather that snows (`snow`): its kind's mean
    /// material, the distant map's seed, and each zone's albedo, worked out once.
    pub fn new(floor: &FloorParams, snow: bool) -> FloorModel {
        let floor = floor.clamped();
        let kind = floor.kind();
        let recipe = FloorModel::recipe(kind);
        let tint = v3(floor.tint);
        let numbers = floor.kind_numbers();
        let snowed = |c: Vec3f, cover: f32| if snow { c.mix(v3(SNOW_ALBEDO), cover) } else { c };
        let (albedo, alt, under) = (v3(recipe.albedo) * tint, v3(recipe.alt) * tint, v3(recipe.under) * tint);
        // the field beside a road and a lake's far bank: Grass at its own default dryness, under this floor's tint
        let field = grass_mean(&FloorModel::recipe(FloorKind::Grass), 0.2, 0.5) * tint;
        let mut model = FloorModel {
            kind,
            seed: floor.seed,
            eye: floor.eye_m,
            reach: floor.detail_m,
            numbers,
            recipe,
            snow,
            vary: true,
            main: albedo,
            main_slope: Vec3f::default(),
            drift: (alt - albedo) * DISTANT_HUE,
            second: Vec3f::default(),
            third: Vec3f::default(),
            road: RoadPath::new(0.0, 0.0, 0.0),
            half: 0.0,
            haze: 1.0,
            verge: VERGE_M,
            summit: SUMMIT_MIN_M,
            field,
            waves: [[0.0; 5]; RIPPLE_WAVES],
            skyline: false,
            sea: None,
            gusts: None,
            rain: None,
            films: damp_films(&floor),
        };
        match kind {
            // (a field's mean depends on the look's depression: `main_at`)
            FloorKind::Grass => {
                // linear in s: worked out at 0 and 1
                let cover = grass_snow(floor.grass_length_m);
                let at0 = snowed(grass_mean(&recipe, floor.dryness, 0.0) * tint, cover);
                let at1 = snowed(grass_mean(&recipe, floor.dryness, 1.0) * tint, cover);
                model.main = at0;
                model.main_slope = at1 - at0;
            }
            FloorKind::Sand => model.main = snowed(albedo, 0.9),
            // fresh powder over snow is snow
            FloorKind::Snow => {}
            FloorKind::Rock => model.main = snowed(albedo.mix(alt, 0.15 + 0.6 * floor.gravel), 0.85),
            FloorKind::Road => {
                let lane = floor.lane_width();
                let width = floor.lanes as f32 * lane;
                // a dashed centre line (12 cm, a third of its length) and two 15 cm edge lines
                let paint = if floor.markings { (0.12 / 3.0 + 2.0 * 0.15) / width } else { 0.0 };
                // under snow the road is white but for two slush tracks a lane (Q5): 2 x 0.5 m of each lane
                model.main = if snow { v3(SNOW_ALBEDO).mix(v3(SLUSH_ALBEDO), 1.0 / lane) } else { albedo.mix(alt, paint) };
                model.second = snowed(under, 1.0);
                // what lies beside the road: the field (Grass), or a material of its own meeting the asphalt's edge
                let beside = road_beside(&floor, tint).unwrap_or(field);
                model.field = beside;
                model.third = snowed(beside, 1.0);
                model.verge = floor.verge_m();
                model.road = RoadPath::new(floor.heading_deg, floor.signed_curve_radius(), floor.curve_start());
                model.half = 0.5 * width;
                model.drift = (field - under) * DISTANT_HUE;
            }
            FloorKind::Plaza => {
                let joint = match floor.pattern() {
                    FloorPattern::Square => 2.0 * 0.01 / floor.tile_m,
                    FloorPattern::Herringbone => 3.0 * 0.01 / floor.tile_m,
                    FloorPattern::Cobbles | FloorPattern::Fan => 2.0 * 0.015 / floor.tile_m,
                    // saw cuts 4 mm wide
                    FloorPattern::Concrete => 2.0 * 0.004 / floor.tile_m,
                }
                .min(0.5);
                // the joints' fill (the GPU's ask takes it as the recipe's `alt`, `kinds_recipe`) and the share of the
                // joints' moss it keeps: open joints moss over, sand less, mortar little
                let (fill, keeps) = match floor.joints() {
                    PlazaJoints::Open => (recipe.alt, 1.0),
                    PlazaJoints::Sand => (PLAZA_SAND, 0.6),
                    PlazaJoints::Mortar => (PLAZA_MORTAR, 0.3),
                };
                model.recipe.alt = fill;
                let moss = keeps * 0.3 * hash01(floor.seed as i32, 7, 3, 0x91a2);
                model.main = snowed(albedo.mix((v3(fill) * tint).mix(under, moss), joint), 1.0);
            }
            FloorKind::Mountain => {
                model.main = snowed(albedo, 0.85);
                model.summit = SUMMIT_MIN_M * (1.0 + hash01(floor.seed as i32, 11, 5, 0x5a17));
                // the far valley is its flat floor: its crops in their shares (the GPU's `crop` and `crops`): green (the
                // Grass field's mean), dark (0.55 of it), ripening (half green, half ripe), ripe (the valley's dry grass at
                // 2.4 times its albedo) and fallow (0.8 of the valley's rock); under the snow line's share on the flat
                // floor, at 0 of Height with its noise under the footprint (the GPU's `fsn`: smoothstep(line - 0.04,
                // line + 0.04, 0), half at Snow line 0, none from 0.04)
                let ripe = alt * 2.4;
                let crops = field * 0.3 + field * (0.55 * 0.12) + field.mix(ripe, 0.5) * 0.16 + ripe * 0.2 + under * (0.8 * 0.22);
                let t = ((0.04 - floor.snow_line) / 0.08).clamp(0.0, 1.0);
                model.second = snowed(crops.mix(v3(SNOW_ALBEDO), t * t * (3.0 - 2.0 * t)), 0.85);
                // (the far parcels' hue drifts between green and ripe)
                model.drift = (ripe - field) * DISTANT_HUE;
            }
            FloorKind::Lake => {
                model.main = if snow { v3(ICE_ALBEDO).mix(v3(SNOW_ALBEDO), ICE_SNOW_SHARE) } else { alt };
                model.second = snowed(albedo, 1.0);
                model.third = snowed(under.mix(field, 0.5), 1.0);
                model.drift = (field - under) * DISTANT_HUE;
                model.waves = ripple_waves(floor.seed);
                model.gusts = Some(GustFrame::new(floor.seed, numbers[2]));
            }
        }
        model.overgrow(&floor, snow);
        model
    }

    /// Overgrowth (`FloorParams::overgrowth`) in the far model: the moss and fine grass along the cracks and joints
    /// (the GPU's `mod.draw.FloorMoss`: its band's mean share in `OVERGROWTH_MOSS`) and the plants' mean cover where
    /// the planes hand over (twice the near reach down; the GPU's `og_mean`, `overgrowth_cover`) in their mean colour
    /// (`overgrowth_plants`) over each zone, and on a road the verge's grass creeping over the asphalt's edge as a
    /// fringe. Under snow weather the cover buries them, but for the edge's creep (the GPU's moves it under snow too);
    /// at 0 nothing changes.
    fn overgrow(&mut self, floor: &FloorParams, snow: bool) {
        let og = floor.overgrowth();
        if !(og > 0.0) {
            return;
        }
        // the verge creeping over the asphalt's edges (the GPU's `og_creep`), as the near relief: not a room's floor;
        // beside grass the shoulder taken over in patches and its ragged fringe of tongues (half of the band) by the
        // crept edge, three quarters of it at Overgrowth 1; beside the rest the shoulder's ground
        if self.kind == FloorKind::Road && floor.beside() != RoadBeside::Room {
            let creep = (og * OVERGROWTH_CREEP_M * 2.0 / (2.0 * self.half)).min(1.0);
            let over = if floor.beside() == RoadBeside::Grass { self.second.mix(self.field, 0.75 * og) } else { self.second };
            self.main = self.main.mix(over, creep);
        }
        if snow {
            return;
        }
        let s = (self.eye / (2.0 * self.reach)).atan().sin();
        let tint = v3(floor.tint);
        // the growth's reach out from a course (the GPU's `mo_reach`, its mean), its band's width (full within the
        // reach, soft to 1.3 of it: `og_band` at k 1) times its cushions' mean (`MOSS_CUSHIONS`), and the share of the
        // courses it has taken by Overgrowth (`mo_take`'s sh before a kind's say, `moss_patch`)
        let reach = 0.006 + 0.02 * og;
        let width_m = 2.3 * reach * MOSS_CUSHIONS;
        let sh = (1.15 * og).min(1.0);
        // (each kind's cells, largest plant, share of rosettes, uprightness, mean say and dryness, as its GPU shader's
        // `og_cell`, `og_size`, `og_rshare`, `og_upright`, `og_dens` and `og_dry`)
        let cover = |cell: f32, size: [f32; 2], rshare: f32, upright: f32, say: f32| overgrowth_cover(cell, size, rshare, upright, say, og, s);
        let moss = |dry: f32| v3(OVERGROWTH_MOSS[0]).mix(v3(OVERGROWTH_MOSS[1]), dry * dry) * tint * OVERGROWTH_MOSS_SHADE;
        match self.kind {
            FloorKind::Road => {
                let wr = floor.wear;
                let width = 2.0 * self.half;
                let lane = floor.lane_width();
                // the band along the cracks (their length a square metre grows with Wear, `ROAD_CRACKS_M`), along the
                // fatigue mesh's lines in the wheel paths (`road_fatigue`) and the edge seams' (5.5 cm and 0.75 reach
                // each, its mean kept as the footprint widens it: `og_mask`), lining a share of them that is the whole
                // of sh by the edges and 0.75 of it from 2.5 m in (`ek`: the half width's mean of that ramp)
                let hw = self.half;
                let ek = (hw.min(0.5) + 0.5 * (hw - 0.5).clamp(0.0, 2.0)) / hw.max(0.01);
                let lined = moss_patch(sh * (0.75 + 0.25 * ek));
                let (fat, fat_m) = road_fatigue(wr, lane);
                let band = (ROAD_CRACKS_M * wr * wr + fat_m) * width_m * lined
                    + 2.0 * (0.055 + 0.75 * reach) * MOSS_CUSHIONS * moss_patch(sh) / width;
                self.main = self.main.mix(moss(0.2), band.min(1.0));
                // the plants in the cracks (their hearts within 0.61 of the largest radius of a course, `og_dens`: a band
                // 1.4 of it wide; by the edges 0.055 and 0.75 of it, 0.8 of the say) and the fatigue mesh's share of
                // the wheel paths (0.6 of it)
                let rp = 0.61 * 0.075 * (0.6 + 0.4 * og);
                let say = (0.5 + 0.5 * wr) * (ROAD_CRACKS_M * wr * wr * 1.4 * rp * 0.75 + 0.6 * fat + 0.8 * 2.0 * (0.055 + 0.75 * rp) / width);
                self.main = self.main.mix(overgrowth_plants(0.7, 0.0, 0.2) * tint, cover(0.25, [0.075, 0.3], 0.7, 0.0, say));
                if floor.beside() == RoadBeside::Grass {
                    // the shoulder taken over by the grass in patches (half of it at Overgrowth 1)
                    self.second = self.second.mix(self.field, 0.5 * og);
                }
            }
            FloorKind::Plaza => {
                // the joints filled and their edges spilled over by the reach either side (the band's share as the
                // units draw it, `plaza_band`), most in open joints
                let joint = self.plaza_joint(floor);
                let fill = match floor.joints() {
                    PlazaJoints::Open => 1.0,
                    PlazaJoints::Sand => 0.7,
                    PlazaJoints::Mortar => 0.35,
                };
                self.main = self.main.mix(moss(0.2), plaza_band(floor.pattern(), floor.tile_m, joint, reach) * MOSS_CUSHIONS * moss_patch(sh * fill));
                // (the joints' half width about a quarter of their share of a tile)
                let jh = (0.25 * joint * floor.tile_m).max(0.0005);
                let cell = (0.5 * floor.tile_m).clamp(0.12, 0.3);
                let size = [0.07f32.min(0.45 * cell), 0.2];
                // (the joint and the band of hearts either side of it, 0.55 of the mean radius each: `og_dens`)
                let say = fill * (joint * (1.0 + 0.55 * 0.675 * size[0] * (0.6 + 0.4 * og) / jh)).min(1.0);
                self.main = self.main.mix(overgrowth_plants(0.75, 0.0, 0.2) * tint, cover(cell, size, 0.75, 0.0, say));
            }
            FloorKind::Rock => {
                // the crevices on the slab's share (their length a square metre 1.25 / stone size, the band their width
                // and 0.7 of the reach either side: `og_crev`), between the stones (`rk_wsm`, their mean share 0.12)
                let st = floor.stone_m.max(0.05);
                let crev = ROCK_SLAB_SHARE * (1.25 / st * 2.0 * (0.8 * st * 0.037 + 0.7 * reach)).min(1.0);
                self.main = self.main.mix(moss(0.25), (crev * (1.0 - 0.12) * MOSS_CUSHIONS * moss_patch(sh)).min(1.0));
                self.main = self.main.mix(overgrowth_plants(0.6, 0.0, 0.25) * tint, cover(0.35, [0.075, 0.28], 0.6, 0.0, 0.05 + 0.2 * ROCK_SLAB_SHARE + 0.1 * (1.0 - ROCK_SLAB_SHARE)));
            }
            // dune grass in stands, bare sand between (every cell of a stand may hold one, about a third of the dune in
            // stands: the GPU's `og_dens`)
            FloorKind::Sand => self.main = self.main.mix(overgrowth_plants(0.0, 1.0, 0.45) * tint, cover(0.45, [0.2, 0.5], 0.0, 1.0, 0.37)),
            _ => {}
        }
    }

    /// The Plaza's joints' share of its ground (its pattern's joints over its tile size).
    fn plaza_joint(&self, floor: &FloorParams) -> f32 {
        match floor.pattern() {
            FloorPattern::Square => 2.0 * 0.01 / floor.tile_m,
            FloorPattern::Herringbone => 3.0 * 0.01 / floor.tile_m,
            FloorPattern::Cobbles | FloorPattern::Fan => 2.0 * 0.015 / floor.tile_m,
            FloorPattern::Concrete => 2.0 * 0.004 / floor.tile_m,
        }
        .min(0.5)
    }

    /// This floor under the skyline (plan hdri-8 §3.10): `skyline` whether the strip is drawn over it (Sky mode, its
    /// block on), and the strip's sea as the app hands it (`ImageSet::skyline_sea`), which the floor carries on below
    /// the strip (D2): water between its edges, a beach SEA_BEACH_M wide on the floor's own side of the shore. A Lake's
    /// water then runs on to the horizon (Q3); a Mountain takes no sea (its valley takes everything below the horizon,
    /// Q4). Without the skyline the floor is as it was, whatever sea is handed.
    pub fn with_skyline(mut self, skyline: bool, sea: Option<Arc<SeaEdge>>) -> FloorModel {
        self.skyline = skyline;
        self.sea = sea.filter(|edge| skyline && self.kind != FloorKind::Mountain && edge.columns() > 0);
        if self.sea.is_some() && self.kind != FloorKind::Lake {
            self.waves = ripple_waves(self.seed);
        }
        self
    }

    /// Whether the skyline is drawn over this floor (`with_skyline`).
    pub fn under_skyline(&self) -> bool {
        self.skyline
    }

    /// The skyline's sea this floor carries on (`with_skyline`), None without one.
    pub fn sea(&self) -> Option<&Arc<SeaEdge>> {
        self.sea.as_ref()
    }

    /// The skyline's sea's bed and its beach as the GPU's ask takes them (`makepad_floor::FloorAsk::sea_ground`): the
    /// Lake's deep water body and its sand, untinted (`sea_over`'s), and the beach's width SEA_BEACH_M.
    pub fn sea_ground() -> [[f32; 4]; 2] {
        let r = FloorModel::recipe(FloorKind::Lake);
        [[r.alt[0], r.alt[1], r.alt[2], 0.0], [r.albedo[0], r.albedo[1], r.albedo[2], SEA_BEACH_M]]
    }

    /// This floor's water under the rain `rain` (`weather::rain_on_water`; the weather lane, 2026-11-09): the Lake's and
    /// the skyline's sea's mirror takes the rain's slopes, its young rings near the eye and the sheen of all of it
    /// (`sample_with`). None leaves the floor as it was, bit for bit.
    pub(crate) fn with_rain(mut self, rain: Option<RainOnWater>) -> FloorModel {
        self.rain = rain;
        self
    }

    /// The air's haze (the sky's atmosphere haze, weathered: 1 a clear day's, more in hazy weather).
    pub fn with_haze(mut self, haze: f32) -> FloorModel {
        self.haze = haze.clamp(0.0, 10.0);
        self
    }

    /// How much of the ground's light reaches the viewer along `d` (unit, below the horizon) by night: the air's
    /// transmittance over the distance to the ground point, exp(-r / V) with V the visibility, NIGHT_VISIBILITY_M in a
    /// clear sky's haze and less in step with the haze; the rest of the look is the night air's own light
    /// (`Env::floor_ground`). By day the atmosphere blends the ground into its bright haze over the lowest degrees
    /// (`Atmosphere::sky_over`); by night that air is dark, and a road lit by the moon runs on to the
    /// horizon rather than fading to black a few degrees down (the owner's "why does the road stop").
    pub fn night_keep(&self, d: Vec3f) -> f32 {
        if !(d.y < 0.0) {
            return 0.0;
        }
        let r = self.eye_m() * (d.x * d.x + d.z * d.z).sqrt() / -d.y;
        (-r * self.haze / NIGHT_VISIBILITY_M).exp()
    }

    /// The road's centre line.
    pub fn road(&self) -> &RoadPath {
        &self.road
    }

    /// A Road's asphalt share at the ground point along `d` (unit, below the horizon): 1 on the road, 0 beside it,
    /// its edge soft over a texel's footprint as `sample_with` draws it; 0 for every other kind and above the
    /// horizon. The hand-over's second material (`FloorMatch`).
    pub fn road_share(&self, d: Vec3f, texel: f32) -> f32 {
        if self.kind != FloorKind::Road {
            return 0.0;
        }
        let s = (-d.y).min(1.0);
        if !(s > 0.0) {
            return 0.0;
        }
        let to_ground = self.eye / s;
        let (x, z) = (d.x * to_ground, d.z * to_ground);
        let r = (d.x * d.x + d.z * d.z).sqrt() * to_ground;
        let foot = if texel > 0.0 { self.eye * texel / (s * s).max(1.0e-30) } else { 0.0 };
        let frame = self.road.frame(x, z);
        let lateral = frame.v.abs();
        let ahead = (x * frame.t[0] + z * frame.t[1]).abs();
        let width = if r > 0.0 { (foot * lateral + r * texel * ahead) / r } else { 0.0 };
        1.0 - soft_step(lateral - self.half, width)
    }

    /// A Lake's ground at the ground point (x, z) `r` metres out (`texel` the lookup's width in radians, `foot` its
    /// footprint along the look, metres): its beach's share and its water's (the rest is the far bank).
    fn lake_ground(&self, x: f32, z: f32, r: f32, texel: f32, foot: f32) -> (f32, f32) {
        let front = LAKE_FRONT_AZ_DEG.to_radians();
        let (fx, fz) = (front.sin(), -front.cos());
        // the angle off straight behind past the shore's sector there (Shore: its share of the back half), from
        // their cosines (the sector's edge is no wider than a texel: the difference of cosines over the
        // sector's sine is the angle there; a sector of no width is all water)
        let cos_behind = if r > 0.0 { (-(x * fx + z * fz) / r).clamp(-1.0, 1.0) } else { 1.0 };
        let sector = self.numbers[2] * std::f32::consts::FRAC_PI_2;
        let past = (sector.cos() - cos_behind) / sector.sin().max(0.05);
        let land = 1.0 - soft_step(past, texel);
        let beach = land * (1.0 - soft_step(r - LAKE_BEACH_M, foot));
        // (under the skyline the water runs on to the horizon: Far shore applies only without it, Q3)
        let water = if self.skyline { 1.0 - land } else { (1.0 - land) * (1.0 - soft_step(r - self.numbers[0], foot)) };
        (beach, water)
    }

    /// The slopes (variance per axis) of the damp film between the puddles at the ground point along `d` (unit, below
    /// the horizon; `Env::damp_sheen`): each ground's own (`damp_films`) in its share of the texel, as `sample_with`
    /// lays them: a Road's asphalt, its verge and what lies beside it, a Lake's beach and its far bank (its water has
    /// no damp ground), the other kinds' one ground. The shares' slopes add as variances: the mixed facets' second
    /// moment. Above the horizon, the main ground's.
    pub(crate) fn damp_slopes_at(&self, d: Vec3f, texel: f32) -> f32 {
        let [main, second, third] = self.films;
        let s = (-d.y).min(1.0);
        if !(s > 0.0) || !matches!(self.kind, FloorKind::Road | FloorKind::Lake) {
            return main;
        }
        let to_ground = self.eye / s;
        let (x, z) = (d.x * to_ground, d.z * to_ground);
        let r = (d.x * d.x + d.z * d.z).sqrt() * to_ground;
        let foot = if texel > 0.0 { self.eye * texel / (s * s).max(1.0e-30) } else { 0.0 };
        if self.kind == FloorKind::Road {
            // (the road's and the verge's edges as `sample_with` softens them)
            let frame = self.road.frame(x, z);
            let lateral = frame.v.abs();
            let ahead = (x * frame.t[0] + z * frame.t[1]).abs();
            let width = if r > 0.0 { (foot * lateral + r * texel * ahead) / r } else { 0.0 };
            let road = 1.0 - soft_step(lateral - self.half, width);
            let verge = (1.0 - soft_step(lateral - self.half - self.verge, width)) - road;
            return main * road + second * verge + third * (1.0 - road - verge);
        }
        let (beach, water) = self.lake_ground(x, z, r, texel, foot);
        let ground = 1.0 - water;
        if ground > 0.0 { (second * beach + third * (ground - beach)) / ground } else { second }
    }

    /// The kind's own numbers as the GPU's ask takes them (`FloorParams::kind_numbers`, as clamped): under the skyline
    /// a Lake's Far shore is its farthest (Q3: the water runs on to the horizon; the planes never reach it).
    pub fn ask_numbers(&self) -> [f32; 8] {
        let mut numbers = self.numbers;
        if self.skyline && self.kind == FloorKind::Lake {
            numbers[0] = FLOOR_FAR_SHORE_RANGE.1;
        }
        numbers
    }

    /// The flat floor's test: no distant map.
    #[cfg(test)]
    pub(crate) fn without_variation(mut self) -> FloorModel {
        self.vary = false;
        self
    }

    pub fn kind(&self) -> FloorKind {
        self.kind
    }

    /// The floor's seed.
    pub fn seed(&self) -> u32 {
        self.seed
    }

    /// The near reach R, metres.
    pub fn reach_m(&self) -> f32 {
        self.reach
    }

    /// The eye over the ground, metres.
    pub fn eye_m(&self) -> f32 {
        self.eye
    }

    /// The kind's material constants this model was made with.
    /// The Grass field beside a road and on a lake's far bank (linear RGB, this floor's tint in, the weather's snow
    /// not): the GPU ask's `field` (plan hdri-8 T3b), so the near relief's field is the far model's.
    pub fn field_albedo(&self) -> [f32; 3] {
        arr3(self.field)
    }

    /// The weather's snow cover as the floor takes it: its albedo (rgb, untinted) and its roughness (the Snow kind's):
    /// the GPU ask's `cover`.
    pub fn snow_cover() -> [f32; 4] {
        let [r, g, b] = SNOW_ALBEDO;
        [r, g, b, FloorModel::recipe(FloorKind::Snow).roughness]
    }

    /// The far model's mean material where the near relief hands over to it, at twice the near reach R (the ground
    /// eye / (2R) down): its albedo (tint and the weather's snow in, `mean_albedo`) and its roughness (the kind's, or
    /// the snow cover's under snow weather): the GPU ask's `far`.
    pub fn far_material(&self) -> [f32; 4] {
        let [r, g, b] = self.mean_albedo((self.eye / (2.0 * self.reach)).atan());
        let roughness = if self.snow { FloorModel::snow_cover()[3] } else { self.recipe.roughness };
        [r, g, b, roughness]
    }

    pub fn kinds_recipe(&self) -> FloorRecipe {
        self.recipe
    }

    /// True when the planes hand over to this far model by twice the near reach on the eye's own ground, where
    /// `FloorMatch` carries their lit mean and fine texture on: every kind but the Mountain, Sand and the Lake. A
    /// Mountain's planes run on to the coarse reach and their top rows are already its far valley (the GPU fades the
    /// valley into the far model's mean by the coarse reach, onto the flat valley floor's distance): a hand-over measured
    /// on the eye's own ground just inside R reads the valley far below against the summit's rock there (a gain that
    /// darkened the whole far valley), and its speckle (cells of 2.5 planes texels, 5.5 tall) runs on with that contrast
    /// over the far ridges and up the haze band to the horizon: vertical stripes. Sand's planes run on over its dunes
    /// to the coarse reach: the hand-over read the dune the viewer stands on against the flat ground, and its gain and
    /// speckle (the ripples' contrast) ran on over the far dunes: tinted bands and vertical stripes. The Lake's ground
    /// just inside R is its beach, and past 2R it has none but the far bank: the pebbles' contrast ran on over the bank's
    /// grass and up the haze as stripes.
    pub fn hands_over_at_reach(&self) -> bool {
        !matches!(self.kind, FloorKind::Mountain | FloorKind::Sand | FloorKind::Lake)
    }

    /// True when the floor mirrors the sky (a Lake's water or ice, the skyline's sea it carries on), so the discs'
    /// reflections are as small as they are.
    pub fn mirrors(&self) -> bool {
        self.kind == FloorKind::Lake || self.sea.is_some()
    }

    /// True when the floor's water has ripples (a Lake that is not frozen, Ripples over 0, or under rain): its sun is
    /// drawn as an analytic glitter (`glitter`), not as the mirrored disc.
    /// The skyline's sea (never frozen: the strip's sea is open water under any weather) ripples at SEA_RIPPLES.
    pub fn rippled(&self) -> bool {
        (self.kind == FloorKind::Lake && !self.snow && (self.numbers[1] > 0.0 || self.rain.is_some()) && self.vary) || (self.sea.is_some() && self.vary)
    }

    /// The water's Ripples: a Lake's own, else the skyline's sea's.
    fn ripples(&self) -> f32 {
        if self.kind == FloorKind::Lake { self.numbers[1] } else { SEA_RIPPLES }
    }

    /// True when a Lake is frozen (the weather snows, §3.8).
    pub fn frozen(&self) -> bool {
        self.kind == FloorKind::Lake && self.snow
    }

    /// The far model's mean albedo of the kind's own ground (its main zone: the road's band, the lake's bed or its ice,
    /// the summit), seen `depression` radians down, without the distant map: what the near relief's mean comes to at
    /// 2R (§4.9's test). Tint and the weather's snow are in.
    pub fn mean_albedo(&self, depression: f32) -> [f32; 3] {
        arr3(self.main_at(depression.sin().clamp(0.0, 1.0)))
    }

    fn main_at(&self, s: f32) -> Vec3f {
        self.main + self.main_slope * s
    }

    /// The far model along `d` (unit, map frame, below the horizon), for a lookup `texel` radians wide (0: an exact
    /// direction, everything at full detail).
    pub fn sample(&self, d: Vec3f, texel: f32) -> FloorFar {
        self.sample_with(d, texel, true)
    }

    /// [`Self::sample`]; `ripples` false leaves the water's ripples unresolved (a held bake's lever): its normal, which
    /// the sky's mirror is turned by, straight up.
    pub fn sample_with(&self, d: Vec3f, texel: f32, ripples: bool) -> FloorFar {
        let up = vec3f(0.0, 1.0, 0.0);
        let s = (-d.y).min(1.0);
        if !(s > 0.0) {
            return FloorFar { albedo: self.main_at(0.0), water: 0.0, ice: 0.0, normal: up, keep: 1.0, shade: 1.0, glint: up, slopes: 0.0, slopes_all: 0.0, rough: 0.0, dist: f32::MAX };
        }
        let h = (d.x * d.x + d.z * d.z).sqrt();
        // the ground point and its distance, and a texel's footprint on the ground: along the look and across it
        let to_ground = self.eye / s;
        let (x, z) = (d.x * to_ground, d.z * to_ground);
        let r = h * to_ground;
        let foot = if texel > 0.0 { self.eye * texel / (s * s).max(1.0e-30) } else { 0.0 };
        let side = r * texel;
        let mut far = FloorFar { albedo: self.main_at(s), water: 0.0, ice: 0.0, normal: up, keep: 1.0, shade: 1.0, glint: up, slopes: 0.0, slopes_all: 0.0, rough: 0.0, dist: to_ground.min(f32::MAX) };
        match self.kind {
            FloorKind::Road => {
                // the ground point in the road's own frame: across it and along it, the road's direction there
                let frame = self.road.frame(x, z);
                let lateral = frame.v.abs();
                let ahead = (x * frame.t[0] + z * frame.t[1]).abs();
                // the edges' softening: the radial footprint across the road and the lateral one along it
                let width = if r > 0.0 { (foot * lateral + side * ahead) / r } else { 0.0 };
                let road = 1.0 - soft_step(lateral - self.half, width);
                let verge = (1.0 - soft_step(lateral - self.half - self.verge, width)) - road;
                far.albedo = self.main * road + self.second * verge + self.third * (1.0 - road - verge);
            }
            FloorKind::Mountain => {
                let plateau = 1.0 - soft_step(r - self.summit, foot);
                // the valley floor, the drop and the eye below: farther at the same depression
                let depth = self.numbers[0] + self.eye;
                let valley_r = h * depth / s;
                let valley_foot = if texel > 0.0 { depth * texel / (s * s).max(1.0e-30) } else { 0.0 };
                // (each zone's distant map only where it shows)
                let (valley, _) = if plateau < 1.0 { self.distant(self.second, d.x * depth / s, d.z * depth / s, valley_foot, VALLEY_SCALE, 1.0) } else { (self.second, 1.0) };
                let haze = (-(valley_r * valley_r + depth * depth).sqrt() / MOUNTAIN_HAZE_M).exp();
                let (summit, shade) = if plateau > 0.0 { self.distant(self.main, x, z, foot, 1.0, self.fade_in(r)) } else { (self.main, 1.0) };
                far.albedo = summit * plateau + valley * (1.0 - plateau);
                far.shade = shade;
                far.keep = plateau + haze * (1.0 - plateau);
                far.dist = far.dist * plateau + (depth / s).min(f32::MAX) * (1.0 - plateau);
                return far;
            }
            FloorKind::Lake => {
                let (beach, water) = self.lake_ground(x, z, r, texel, foot);
                let ground = (1.0 - water).max(0.0);
                // the lake's own share keeps its bed's or its ice's colour unvaried: what shows there is its mirror
                let (dry, shade) = if ground > 0.0 { self.distant((self.second * beach + self.third * (ground - beach)) * (1.0 / ground), x, z, foot, 1.0, self.fade_in(r)) } else { (self.main, 1.0) };
                far.albedo = dry * ground + self.main * water;
                far.shade = shade;
                if self.snow {
                    far.ice = water;
                } else if water > 0.0 || r < 2.0 * self.reach {
                    far.water = water;
                    // (a held bake leaves the ripples it resolves level; its glitter follows the wavelets as a released
                    // one's does). The ripples also run under the shore's sector within the planes' 2R, where their own
                    // waterline meanders past the wedge's edge: their water there takes this normal too (only the
                    // water reads it), so it ripples on across the edge's column instead of lying flat beside it
                    // The mirror: the ripples as the gusts there raise them (`gust`), and the slopes the texel leaves
                    // unresolved as they raise them, which the mirror's facets spread by (`rough`, `mirror_lobe`) round
                    // their visible mean (`visible_tilt`), held or released alike: the far lake is the same in a drag
                    // and once released. The sun's glitter keeps the lake's mean ripples: a gust lane resolved at one
                    // bake width and averaged at another would change its light, which holds across widths (`glitter`)
                    let (normal, slopes) = self.ripple_normal(x, z, foot, side);
                    // (no ripples, no gusts to scale)
                    let gust = if slopes > 0.0 || normal.y < 1.0 { self.gust(x, z, foot, side) } else { 1.0 };
                    let mut centre = if ripples { vec3f(gust * normal.x / normal.y, 1.0, gust * normal.z / normal.y).normalize() } else { up };
                    far.rough = slopes * gust * gust;
                    (far.glint, far.slopes, far.slopes_all) = (normal, slopes, self.all_slopes());
                    // Rain (the weather lane): its young rings the texel resolves turn the mirror with the ripples (a held
                    // bake leaves them level, as it does the ripples), the rest of its slopes spread it; the glitter takes
                    // all of the rain's slopes as unresolved
                    if let Some(rain) = &self.rain {
                        let (gx, gz, left) = rain.at(x, z, foot, side);
                        if ripples {
                            centre = vec3f(centre.x / centre.y - gx, 1.0, centre.z / centre.y - gz).normalize();
                        }
                        far.rough += left;
                        far.slopes += rain.all();
                        far.slopes_all += rain.all();
                    }
                    far.normal = visible_tilt(centre, far.rough, x, z, s);
                }
                self.sea_over(&mut far, d, s, texel, (x, z, foot, side), ripples);
                return far;
            }
            _ => {}
        }
        (far.albedo, far.shade) = self.distant(far.albedo, x, z, foot, 1.0, self.fade_in(r));
        self.sea_over(&mut far, d, s, texel, (x, z, foot, side), ripples);
        far
    }

    /// The skyline's sea over `far` along `d` (`s` the sine of its depression, `texel` the lookup's width in radians;
    /// `ground` the ground point, its footprint along the look and across it), where this floor carries it on (D2):
    /// open water between the strip's far and near edge there (`SeaEdge::at`, by angle, each edge softened over the
    /// texel), over the deep water's dark body (the Lake's), rippled at SEA_RIPPLES (or a Lake's Ripples); a beach of the
    /// Lake's pebbles and sand SEA_BEACH_M wide inward of the shore; both over the floor's dry share only (a Lake's water
    /// or ice stays its own). Nothing without a sea.
    fn sea_over(&self, far: &mut FloorFar, d: Vec3f, s: f32, texel: f32, ground: (f32, f32, f32, f32), ripples: bool) {
        let Some(edge) = &self.sea else { return };
        let dry = (1.0 - far.water - far.ice).max(0.0);
        if !(dry > 0.0) {
            return;
        }
        let [open_from, near] = edge.at(ibl::dir_to_equirect_uv([d.x, d.y, d.z])[0]);
        if !(near > open_from) {
            return;
        }
        let al = s.min(1.0).asin();
        let cover = |edge: f32| soft_step(al - edge, texel);
        let open = if open_from > 0.0 { cover(open_from) } else { 1.0 };
        let water = (open - cover(near)).max(0.0) * dry;
        // the beach: from the shore inward by SEA_BEACH_M of the floor's own ground (all of it when the shore is nearer)
        let shore_r = self.eye / near.tan().max(1.0e-6);
        let inner = if shore_r > SEA_BEACH_M { (self.eye / (shore_r - SEA_BEACH_M)).atan() } else { std::f32::consts::FRAC_PI_2 + 1.0 };
        let beach = (cover(near) - cover(inner)).max(0.0) * dry;
        if !(water > 0.0 || beach > 0.0) {
            return;
        }
        let recipe = FloorModel::recipe(FloorKind::Lake);
        let bed = v3(recipe.alt);
        let sand = if self.snow { v3(SNOW_ALBEDO) } else { v3(recipe.albedo) };
        far.albedo = far.albedo * (1.0 - water - beach) + bed * water + sand * beach;
        if water > 0.0 {
            far.water += water;
            if far.slopes_all == 0.0 {
                let (x, z, foot, side) = ground;
                let (normal, slopes) = self.ripple_normal(x, z, foot, side);
                far.normal = if ripples { normal } else { vec3f(0.0, 1.0, 0.0) };
                (far.glint, far.slopes, far.slopes_all) = (normal, slopes, self.all_slopes());
                // rain on the sea (the weather lane): far off, its slopes are all a sheen, which the mirror's facets
                // spread it by round their visible mean
                if let Some(rain) = &self.rain {
                    far.rough = rain.all();
                    far.slopes += rain.all();
                    far.slopes_all += rain.all();
                    far.normal = visible_tilt(far.normal, far.rough, x, z, s);
                }
            }
        }
    }

    /// `c` under the distant map at ground point (x, z), its octaves band-limited to a footprint `foot` (§3.9);
    /// `scale` stretches the wavelengths (a Mountain's valley). Its mean over the ground is `c` (value noise is 0.5 on
    /// average), so the far model stays the near relief's mean.
    /// Also its brightness factor.
    /// `fade` (0..1, `fade_in`) is how much of it shows: the distant map fades in with distance (§3.9).
    fn distant(&self, c: Vec3f, x: f32, z: f32, foot: f32, scale: f32, fade: f32) -> (Vec3f, f32) {
        if !self.vary || !(fade > 0.0) {
            return (c, 1.0);
        }
        let mut bright = 0.0;
        let mut hue = 0.0;
        let per_foot = if foot > 0.0 { 1.0 / foot } else { f32::INFINITY };
        for k in 0..3 {
            let wavelength = DISTANT_WAVELENGTHS_M[k] * scale;
            let weight = band_ratio(wavelength * per_foot);
            if weight > 0.0 {
                let seed = self.seed.wrapping_mul(0x9e37_79b9) ^ (0x51ed_0000 + k as u32);
                let inv = 1.0 / wavelength;
                let n = 2.0 * cell_noise(x * inv, z * inv, seed) - 1.0;
                bright += weight * DISTANT_AMPLITUDES[k] * n;
                // the hue drifts with the largest patches' brightness: the brighter patches the drier
                if k == 0 {
                    hue = weight * n;
                }
            }
        }
        let (bright, hue) = (bright * fade, hue * fade);
        let out = c * (1.0 + bright) + self.drift * hue;
        (vec3f(out.x.max(0.0), out.y.max(0.0), out.z.max(0.0)), 1.0 + bright)
    }

    /// The variance (per axis) of all the ripples' slopes: a wave of slope a has a^2 / 2 along it and its bend
    /// (RIPPLE_BEND_RATE x RIPPLE_BEND)^2 / 2 of that across it, a^2 / 4 per axis over its turns.
    fn all_slopes(&self) -> f32 {
        let amount = if self.vary { self.ripples().max(0.0) } else { 0.0 };
        let bent = 1.0 + 0.5 * (RIPPLE_BEND_RATE * RIPPLE_BEND) * (RIPPLE_BEND_RATE * RIPPLE_BEND);
        RIPPLE_WAVES as f32 * 0.25 * (amount * RIPPLE_SLOPE) * (amount * RIPPLE_SLOPE) * bent
    }

    /// The kind's own ground texture at ground point (x, z), as its GPU shader draws its largest octaves (the same
    /// wavelengths and amplitudes: Grass's mottle, the Rock summit's tone patches), each faded at its Nyquist for a
    /// footprint `foot` (as the GPU's `lod` with the released samples): what the far model carries on where the planes
    /// hand over to it (`Env::floor_carry`), so the field's texture keeps its feature scale and contrast across the fade.
    /// The factor (mean 1) and its standard deviation. (1, 0) for the kinds without one.
    pub fn fine_texture(&self, x: f32, z: f32, foot: f32) -> (f32, f32) {
        let s = self.numbers[0].clamp(0.05, 2.0);
        let octaves: &[(f32, f32)] = match self.kind {
            FloorKind::Grass => &[(1.6, 0.2), (0.45, 0.15), (0.13, 0.12)],
            FloorKind::Rock => &[(2.5, 0.2), (9.0, 0.14), (5.0, 0.1)],
            _ => &[],
        };
        let scale = if self.kind == FloorKind::Rock { s } else { 1.0 };
        let (mut f, mut var) = (1.0f32, 0.0f32);
        for (k, &(wavelength, amplitude)) in octaves.iter().enumerate() {
            let l = wavelength * scale;
            // (the GPU's lod with four samples a texel: whole over two footprints, gone under one)
            let keep = if foot > 0.0 { smooth(1.0, 2.0, l / foot) } else { 1.0 };
            if keep > 0.0 {
                let seed = self.seed.wrapping_mul(0x27d4_eb2d) ^ (0x7e57_0000 + k as u32);
                // (value noise about its mean, scaled to the gradient noise's spread the GPU draws with)
                let n = 2.0 * (cell_noise(x / l, z / l, seed) - 0.5) * 1.6;
                f += keep * amplitude * n;
                var += (keep * amplitude * 0.42) * (keep * amplitude * 0.42);
            }
        }
        (f.max(0.0), var.sqrt())
    }

    /// How much of the distant map shows at ground distance `r` (§2, §3.9): none within the near reach R, where the
    /// near relief carries the ground's own detail, all of it from 2R, fading in over the fade zone between.
    fn fade_in(&self, r: f32) -> f32 {
        smooth(self.reach, 2.0 * self.reach, r)
    }

    /// The gusts' factor on the Lake's ripples at ground point (x, z) (GUST_CELLS_M; 1 for the skyline's sea on the other
    /// kinds): two octaves of value noise in a frame turned to the ripples' seeded wind (`ripple_waves`), drawn out
    /// along it, each band-limited to the footprint as it lies in the lanes' frame (`foot` along the look, `side`
    /// across it: a lane along the look stays where one across it is long gone); past the footprint the factor's
    /// mean square, 1 (the slopes' variance it scales holds). In a shore's lee it falls to GUST_LEE (`lee`).
    fn gust(&self, x: f32, z: f32, foot: f32, side: f32) -> f32 {
        let Some(frame) = self.gusts.as_ref().filter(|_| self.vary) else {
            return 1.0;
        };
        let [wx, wz] = frame.wind;
        let r = (x * x + z * z).sqrt();
        let (vx, vz) = if r > 0.0 { (x / r, z / r) } else { (0.0, 1.0) };
        // the footprint's reach in the lanes' cells, along the wind and across it
        let (va, vc) = (vx * wx + vz * wz, vz * wx - vx * wz);
        let along = ((va * foot) * (va * foot) + (vc * side) * (vc * side)).sqrt() / GUST_CELLS_M[0];
        let across = ((vc * foot) * (vc * foot) + (va * side) * (va * side)).sqrt() / GUST_CELLS_M[1];
        let cells = along.max(across);
        let keep = |scale: f32| if cells > 0.0 { smooth(GUST_GONE, GUST_FULL, scale / cells) } else { 1.0 };
        let (k1, k2) = (keep(1.0), keep(0.4));
        let lee = self.lee(x, z, frame);
        if !(k1 > 0.0) {
            return lee;
        }
        let (u, v) = (x * wx + z * wz, z * wx - x * wz);
        let seed = self.seed.wrapping_mul(0x2545_f491);
        let n1 = cell_noise(u / GUST_CELLS_M[0], v / GUST_CELLS_M[1], seed ^ 0x6a51_0000);
        let n2 = cell_noise(u / (0.4 * GUST_CELLS_M[0]) + 5.3, v / (0.4 * GUST_CELLS_M[1]) + 1.7, seed ^ 0x6a52_0000);
        let n = 0.5 + 0.7 * (n1 - 0.5) + 0.3 * (n2 - 0.5) * k2;
        let g = GUST_SCALE * (0.35 + 1.1 * smooth(0.2, 0.8, n));
        (1.0 + (g * g - 1.0) * k1).max(0.0).sqrt() * lee
    }

    /// The Lake's lee at ground point (x, z) for its gusts' wind (`frame`): GUST_LEE where the shore up-wind (the far
    /// shore at Far shore, without the skyline; the shore's sector behind the viewer) is within GUST_LEE_M.0, rising
    /// to 1 by GUST_LEE_M.1 of open water up-wind. The wind wraps round the point of land under the viewer's feet, so the
    /// sector shelters the water only as much as the wind crosses its edge GUST_LEE_M.1 or more from the point (none at
    /// the point: the lee has no edge along the line the wind passes the point on).
    /// A point inside the sector is its land, or the water of a bay the GPU's meander cuts into it within 2R (`wig` in
    /// lake.rs: up to about 1.8 m in from the straight edge, which `sample_with` gives a mirror too): it takes the lee of
    /// the open water just outside the nearer edge, so the shelter runs on across the edge's line. A bay lies in the
    /// land's lee where the wind blows off the land there and open to it where the wind blows in, as the water beside it.
    fn lee(&self, x: f32, z: f32, frame: &GustFrame) -> f32 {
        let [wx, wz] = frame.wind;
        let (mut x, mut z) = (x, z);
        if let Some(edges) = &frame.edges {
            let inside = edges.map(|[_, _, nx, nz]| x * nx + z * nz);
            if inside[0] >= 0.0 && inside[1] >= 0.0 {
                let k = if inside[0] <= inside[1] { 0 } else { 1 };
                let out = inside[k] + LEE_EDGE_OUT_M;
                (x, z) = (x - out * edges[k][2], z - out * edges[k][3]);
            }
        }
        let open = |fetch: f32| smooth(GUST_LEE_M.0, GUST_LEE_M.1, fetch);
        let mut shelter = 0.0f32;
        let pw = x * wx + z * wz;
        let r2 = x * x + z * z;
        if !self.skyline {
            let f = self.numbers[0];
            let q = pw * pw - r2 + f * f;
            if q >= 0.0 && r2 < f * f {
                shelter = shelter.max(1.0 - open(pw + q.sqrt()));
            }
        }
        // the sector's two edges: rays from the eye's foot, Shore x 90 degrees either side of straight behind
        if let Some(edges) = &frame.edges {
            for &[bx, bz, _, _] in edges {
                // where the wind up-wind of p crosses the edge: p - s w = t b, s up-wind of p, t out from the point
                let det = wx * bz - wz * bx;
                if det.abs() > 1.0e-6 {
                    let s = (x * bz - z * bx) / det;
                    let t = (wx * z - wz * x) / det;
                    if s >= 0.0 && t >= 0.0 {
                        shelter = shelter.max((1.0 - open(s)) * smooth(0.0, GUST_LEE_M.1, t));
                    }
                }
            }
        }
        1.0 - (1.0 - GUST_LEE) * shelter
    }

    /// The water's normal at ground point (x, z) (§3.8): the seeded wind waves of Ripples' slope, their crests bent, each
    /// band-limited to the texel's footprint along its own direction (`foot` along the look, `side` across it: a wave
    /// running across the look stays while one running along it is long gone, so far water keeps its horizontal
    /// streaks); straight up at Ripples 0 and where every wave is finer than the texel. With it the variance (per axis)
    /// of the slopes the normal leaves out, which the glitter spreads its lobe by: the waves the band limit took and how
    /// much the kept ones turn within the footprint, so the sun's glitter follows the ripples the texel shows and never
    /// sparkles from texel to texel.
    fn ripple_normal(&self, x: f32, z: f32, foot: f32, side: f32) -> (Vec3f, f32) {
        let up = vec3f(0.0, 1.0, 0.0);
        let amount = self.ripples();
        if !(amount > 0.0) || !self.vary {
            return (up, 0.0);
        }
        let bent = 1.0 + 0.5 * (RIPPLE_BEND_RATE * RIPPLE_BEND) * (RIPPLE_BEND_RATE * RIPPLE_BEND);
        let quarter = 0.25 * (amount * RIPPLE_SLOPE) * (amount * RIPPLE_SLOPE) * bent;
        let mut left = 0.0f32;
        let r = (x * x + z * z).sqrt();
        let (vx, vz) = if r > 0.0 { (x / r, z / r) } else { (0.0, 1.0) };
        let (mut gx, mut gz) = (0.0f32, 0.0f32);
        for k in 0..RIPPLE_WAVES {
            let [kx, kz, wavenumber, phase, bend] = self.waves[k];
            // the footprint along the wave's own direction
            let (a, c) = (kx * vx + kz * vz, kx * vz - kz * vx);
            let f = ((a * foot) * (a * foot) + (c * side) * (c * side)).sqrt();
            let weight = if f > 0.0 { smooth(RIPPLE_GONE, RIPPLE_FULL, RIPPLE_WAVELENGTHS_M[k] / f) } else { 1.0 };
            // (what the band limit took, and the kept share's turn over the footprint: wavenumber x footprint radians)
            let turn = wavenumber * f;
            left += quarter * ((1.0 - weight * weight) + weight * weight * (turn * turn).min(2.0));
            if weight > 0.0 {
                // its crests bent along their length: the phase shifted by a slow wave across it
                let across = (z * kx - x * kz) * wavenumber * RIPPLE_BEND_RATE + bend;
                let along = (x * kx + z * kz) * wavenumber + phase + RIPPLE_BEND * across.sin();
                let slope = weight * amount * RIPPLE_SLOPE * along.cos();
                let tilt = RIPPLE_BEND * RIPPLE_BEND_RATE * across.cos();
                gx += slope * (kx - tilt * kz);
                gz += slope * (kz + tilt * kx);
            }
        }
        (vec3f(-gx, 1.0, -gz).normalize(), left)
    }
}

// ---- plan hdri-8 T2: the planes the GPU renders, the floor's light, the composition --------------------------------

/// Bytes a packed plane texel takes (§4.2), as makepad-floor packs it:
///
/// | bytes | value | encoding |
/// |---|---|---|
/// | 0..3 | albedo x AO, rgb | 8-bit sRGB-encoded |
/// | 3 | the key light's term (0..1) | 8-bit linear |
/// | 4 | hollow | 8-bit linear |
/// | 5 | water share | 8-bit linear |
/// | 6 | ice share | 8-bit linear |
/// | 7 | roughness | 8-bit linear |
/// | 8 | sparkle | 8-bit linear |
/// | 9 | 0 (spare) | |
/// | 10..12 | normal x | 16-bit snorm, little-endian |
/// | 12..14 | normal z | 16-bit snorm, little-endian |
/// | 14..16 | distance (m) | IEEE half, little-endian |
///
/// The normal's y is implied (facing up).
pub const FLOOR_TEXEL_BYTES: usize = 16;

/// One plane texel, unpacked (§4.2: every value the mean of the texel's samples).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FloorTexel {
    /// Albedo x ambient occlusion, linear RGB (tint and the weather's snow in).
    pub albedo: [f32; 3],
    /// The key light's term: the mean of max(0, n.l) x its shadow, plus grass's transmission, 0..1.
    pub key: f32,
    /// How far the texel lies below its neighbourhood, 0..1 (the puddles' threshold is applied to it on the CPU).
    pub hollow: f32,
    pub water: f32,
    pub ice: f32,
    pub roughness: f32,
    pub sparkle: f32,
    /// The surface's normal, unit, y up.
    pub normal: [f32; 3],
    /// The distance to what the texel shows, metres.
    pub dist: f32,
}

/// The 8-bit sRGB code's linear values, built once.
fn srgb_table() -> &'static [f32; 256] {
    static TABLE: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        std::array::from_fn(|i| {
            let x = i as f32 / 255.0;
            if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
        })
    })
}

/// 0..1 to a byte (NaN and below 0 to 0).
fn unorm8(x: f32) -> u8 {
    if x > 0.0 { (x.min(1.0) * 255.0 + 0.5) as u8 } else { 0 }
}

/// -1..1 to a 16-bit snorm (NaN to 0).
fn snorm16(x: f32) -> i16 {
    if x.is_nan() { 0 } else { (x.clamp(-1.0, 1.0) * 32767.0).round() as i16 }
}

/// An IEEE half's value, subnormals included.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let (e, m) = ((h >> 10) & 0x1f, (h & 0x3ff) as u32);
    match e {
        0 => sign * m as f32 * (1.0 / 16_777_216.0),
        31 => if m == 0 { sign * f32::INFINITY } else { f32::NAN },
        _ => f32::from_bits(((h as u32 & 0x8000) << 16) | ((e as u32 + 112) << 23) | (m << 13)),
    }
}

/// The nearest IEEE half to `x` (ties to even); past the largest half, infinity; NaN stays NaN.
pub fn f32_to_f16(x: f32) -> u16 {
    let bits = x.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let a = x.abs();
    if a.is_nan() {
        return 0x7e00;
    }
    if a >= 65520.0 {
        return sign | 0x7c00;
    }
    if a < 6.103_515_6e-5 {
        return sign | (a * 16_777_216.0).round_ties_even() as u16;
    }
    let e = ((bits >> 23) & 0xff) as i32 - 112;
    let m = bits & 0x7f_ffff;
    let mut h = ((e as u32) << 10) | (m >> 13);
    let rest = m & 0x1fff;
    if rest > 0x1000 || (rest == 0x1000 && h & 1 == 1) {
        h += 1;
    }
    sign | h as u16
}

impl FloorTexel {
    /// The texel packed (FLOOR_TEXEL_BYTES' table): values out of range are held into it, NaN is 0, a distance that is
    /// not finite or not above 0 is 0.
    pub fn pack(&self) -> [u8; FLOOR_TEXEL_BYTES] {
        let mut o = [0u8; FLOOR_TEXEL_BYTES];
        for i in 0..3 {
            let x = self.albedo[i];
            o[i] = unorm8(if x <= 0.003_130_8 { 12.92 * x } else { 1.055 * x.min(1.0).powf(1.0 / 2.4) - 0.055 });
        }
        for (k, x) in [self.key, self.hollow, self.water, self.ice, self.roughness, self.sparkle].into_iter().enumerate() {
            o[3 + k] = unorm8(x);
        }
        o[10..12].copy_from_slice(&snorm16(self.normal[0]).to_le_bytes());
        o[12..14].copy_from_slice(&snorm16(self.normal[2]).to_le_bytes());
        let d = if self.dist.is_finite() && self.dist > 0.0 { f32_to_f16(self.dist) } else { 0 };
        o[14..16].copy_from_slice(&d.to_le_bytes());
        o
    }

    /// A packed texel's values; always finite (an infinite or NaN distance is 0, a normal past the unit disc is level).
    pub fn unpack(b: &[u8; FLOOR_TEXEL_BYTES]) -> FloorTexel {
        let s = srgb_table();
        let u = |i: usize| b[i] as f32 * (1.0 / 255.0);
        let sn = |i: usize| (i16::from_le_bytes([b[i], b[i + 1]]) as f32 * (1.0 / 32767.0)).clamp(-1.0, 1.0);
        let (nx, nz) = (sn(10), sn(12));
        let dist = f16_to_f32(u16::from_le_bytes([b[14], b[15]]));
        FloorTexel {
            albedo: [s[b[0] as usize], s[b[1] as usize], s[b[2] as usize]],
            key: u(3),
            hollow: u(4),
            water: u(5),
            ice: u(6),
            roughness: u(7),
            sparkle: u(8),
            normal: unit_normal(nx, nz),
            dist: if dist.is_finite() && dist > 0.0 { dist } else { 0.0 },
        }
    }
}

/// The unit normal whose x and z are `nx` and `nz`, y up; level where they leave no room for y.
fn unit_normal(nx: f32, nz: f32) -> [f32; 3] {
    let yy = 1.0 - nx * nx - nz * nz;
    if yy > 0.0 {
        [nx, yy.sqrt(), nz]
    } else {
        let l = (nx * nx + nz * nz).sqrt();
        if l > 0.0 { [nx / l, 0.0, nz / l] } else { [0.0, 1.0, 0.0] }
    }
}

/// The planes (§4.2): plain data, like `SkylineBand`. `w` columns over the whole turn in the map's own frame (column x
/// centred at azimuth ((x + 0.5) / w - 0.5) x 360 degrees, a bake's own columns), `h` rows of 360 / w degrees from
/// `top_deg` (the top row's top edge, on a bake's row grid) down, the last ending at nadir; each texel packed
/// (FLOOR_TEXEL_BYTES' table). Never wider than FLOOR_PLANES_MAX_WIDTH (Q9): wider bakes read them bilinearly.
#[derive(Clone, Debug, PartialEq)]
pub struct FloorPlanes {
    w: usize,
    h: usize,
    top_deg: f32,
    eye_m: f32,
    texels: Vec<[u8; FLOOR_TEXEL_BYTES]>,
    /// The near reach they were rendered with (metres; the quality's: a held ask's is half the floor's), 0 when the
    /// host did not say (the floor's own is then taken): where the far model takes over (`FloorMatch`).
    reach: f32,
}

/// The widest planes (Q9).
pub const FLOOR_PLANES_MAX_WIDTH: usize = 4096;

impl FloorPlanes {
    /// The handoff, made safe: `w` at least 2, `h` at least 1, the texels cut or padded (with zeros: black, level,
    /// dry) to w x h, `top_deg` held to -90..=0 (0 where it is not finite), an eye that is not finite or not above 0
    /// read as 0.
    pub fn new(w: usize, h: usize, top_deg: f32, eye_m: f32, mut texels: Vec<[u8; FLOOR_TEXEL_BYTES]>) -> FloorPlanes {
        let (w, h) = (w.max(2), h.max(1));
        texels.resize(w * h, [0; FLOOR_TEXEL_BYTES]);
        let top_deg = if top_deg.is_finite() { top_deg.clamp(-90.0, 0.0) } else { 0.0 };
        let eye_m = if eye_m.is_finite() && eye_m > 0.0 { eye_m } else { 0.0 };
        FloorPlanes { w, h, top_deg, eye_m, texels, reach: 0.0 }
    }

    /// These planes, rendered with the near reach `reach_m` (the ask's, the quality's share in): where they hand over
    /// to the far model. Not finite or not above 0: unknown.
    pub fn with_reach(mut self, reach_m: f32) -> FloorPlanes {
        self.reach = if reach_m.is_finite() && reach_m > 0.0 { reach_m } else { 0.0 };
        self
    }

    /// The near reach they were rendered with (metres), 0 when unknown.
    pub fn reach(&self) -> f32 {
        self.reach
    }

    /// Planes made on the CPU (tests; a host's synthetic planes): `texel(d)` at each texel's centre direction (map
    /// frame).
    pub fn from_fn(w: usize, h: usize, top_deg: f32, eye_m: f32, texel: impl Fn(Vec3f) -> FloorTexel) -> FloorPlanes {
        let (w, h) = (w.max(2), h.max(1));
        let row = 360.0 / w as f32;
        let mut texels = Vec::with_capacity(w * h);
        for y in 0..h {
            let el = top_deg - (y as f32 + 0.5) * row;
            for x in 0..w {
                let az = ((x as f32 + 0.5) / w as f32 - 0.5) * 360.0;
                texels.push(texel(super::dir_from_az_el(az, el.max(-90.0))).pack());
            }
        }
        FloorPlanes::new(w, h, top_deg, eye_m, texels)
    }

    /// The rows of planes `w` wide that reach from nadir up past `depression_deg`: (h, top_deg), the top edge on a
    /// bake's row grid (edges at 90 - k 360 / w degrees).
    pub fn rows_for(w: usize, depression_deg: f32) -> (usize, f32) {
        let w = w.max(2);
        let d = 360.0 / w as f64;
        let k = ((90.0 + (depression_deg as f64).clamp(0.0, 90.0)) / d).floor() as usize;
        let k = k.min(w / 2 - 1);
        (w / 2 - k, (90.0 - k as f64 * d) as f32)
    }

    pub fn width(&self) -> usize {
        self.w
    }

    pub fn height(&self) -> usize {
        self.h
    }

    pub fn top_deg(&self) -> f32 {
        self.top_deg
    }

    pub fn eye_m(&self) -> f32 {
        self.eye_m
    }

    /// The bytes the planes hold.
    pub fn bytes(&self) -> usize {
        self.texels.len() * FLOOR_TEXEL_BYTES
    }

    /// Texel (x, y), unpacked (x wraps, y is held to the rows).
    pub fn texel(&self, x: usize, y: usize) -> FloorTexel {
        FloorTexel::unpack(&self.texels[y.min(self.h - 1) * self.w + x % self.w])
    }

    /// The planes along `d` (unit, map frame), read bilinearly between texel centres (a bake at the planes' width
    /// reads each texel at its centre), and the share of the lookup they hold: 1 from two rows under their top edge
    /// down, fading to 0 at the top edge, so they hand over to the far model without a seam. None above their top.
    pub fn sample(&self, d: Vec3f) -> Option<(FloorTexel, f32)> {
        let el = d.y.clamp(-1.0, 1.0).asin().to_degrees();
        let row = 360.0 / self.w as f32;
        let fy = (self.top_deg - el) / row - 0.5;
        if !(fy > -0.5) {
            return None;
        }
        let share = smooth(-0.5, 1.5, fy);
        let u = 0.5 + d.x.atan2(-d.z) * (1.0 / std::f32::consts::TAU);
        let fx = u * self.w as f32 - 0.5;
        let fy = fy.clamp(0.0, (self.h - 1) as f32);
        // within a thousandth of a texel of a centre (a bake at the planes' own width) the texel itself: one unpack, not
        // four
        let snap = |f: f32| {
            let r = f.round();
            if (f - r).abs() < 1.0e-3 { r } else { f }
        };
        let (fx, fy) = (snap(fx), snap(fy));
        let (x0, tx) = (fx.floor(), fx - fx.floor());
        let (y0, ty) = (fy.floor(), fy - fy.floor());
        let x0 = (x0 as i64).rem_euclid(self.w as i64) as usize;
        let y0 = y0 as usize;
        let (x1, y1) = ((x0 + 1) % self.w, (y0 + 1).min(self.h - 1));
        let t = if tx == 0.0 && ty == 0.0 {
            self.texel(x0, y0)
        } else {
            let mut acc = Bilinear::default();
            for (x, y, weight) in [(x0, y0, (1.0 - tx) * (1.0 - ty)), (x1, y0, tx * (1.0 - ty)), (x0, y1, (1.0 - tx) * ty), (x1, y1, tx * ty)] {
                if weight > 0.0 {
                    acc.add(&self.texel(x, y), weight);
                }
            }
            acc.texel()
        };
        Some((t, share))
    }
}

/// The weighted sum of a few texels (`FloorPlanes::sample`'s four), its normal made unit once at the end.
#[derive(Default)]
struct Bilinear {
    v: [f32; 13],
}

impl Bilinear {
    fn add(&mut self, t: &FloorTexel, w: f32) {
        let x = [t.albedo[0], t.albedo[1], t.albedo[2], t.key, t.hollow, t.water, t.ice, t.roughness, t.sparkle, t.normal[0], t.normal[1], t.normal[2], t.dist];
        for (a, x) in self.v.iter_mut().zip(x) {
            *a += x * w;
        }
    }

    fn texel(&self) -> FloorTexel {
        let v = &self.v;
        let len = (v[9] * v[9] + v[10] * v[10] + v[11] * v[11]).sqrt();
        FloorTexel {
            albedo: [v[0], v[1], v[2]],
            key: v[3],
            hollow: v[4],
            water: v[5],
            ice: v[6],
            roughness: v[7],
            sparkle: v[8],
            normal: if len > 0.0 { [v[9] / len, v[10] / len, v[11] / len] } else { [0.0, 1.0, 0.0] },
            dist: v[12],
        }
    }
}

/// The GPU's integer hash (makepad-floor's `hu`), for `fade_start`: noise.rs's lowbias32, the same constants.
use crate::noise::hash_u32 as gpu_hash;

/// A periodic value noise round the turn (makepad-floor's `ring`): u 0..1, n knots, layer k; 0..1.
fn ring(u: f32, n: f32, k: u32, seed: u32) -> f32 {
    let f = u * n;
    let i = f.floor();
    let t = f - i;
    let s = t * t * (3.0 - 2.0 * t);
    let a = i - n * (i / n).floor();
    let b = (i + 1.0) - n * ((i + 1.0) / n).floor();
    let salt = k.wrapping_mul(1000).wrapping_add(seed.wrapping_mul(7919));
    let h = |c: f32| (gpu_hash((c as u32).wrapping_add(salt)) >> 8) as f32 * (1.0 / 16_777_216.0);
    h(a) + (h(b) - h(a)) * s
}

/// Where the planes' near share starts to fade along `d` (makepad-floor's `fade_start`): a share 0.6..1 of the near
/// reach wandering with the azimuth, seeded, so the hand-over is long and never a circle; it ends at twice the reach.
pub fn fade_start(d: Vec3f, seed: u32) -> f32 {
    let u = 0.5 + d.x.atan2(-d.z) * (1.0 / std::f32::consts::TAU);
    0.6 + 0.4 * (0.6 * ring(u, 7.0, 1, seed) + 0.4 * ring(u, 19.0, 2, seed))
}

/// Azimuth bins of the far model's hand-over (`FloorMatch`): 64, 5.6 degrees each.
const MATCH_BINS: usize = 64;
/// The speckle's cells, in planes texels, and their weights: the fine texture the far model carries on with
/// (`FloorMatch::speckle`).
const SPECKLE_CELLS: [(f32, f32); 2] = [(1.0, 0.7), (2.5, 0.5)];
/// The standard deviation of the speckle's raw noise (the two octaves above about their mean): the contrast it
/// carries is the measured one over this.
const SPECKLE_SD: f32 = 0.115;

/// How the far model carries on where the planes hand over to it (the look pass's shared fade, plan hdri-8). The
/// near relief's own lit mean and texture differ from the far model's flat ground (a grass canopy's light depends
/// on the sun and the look; its texture is the blades'), so where the planes fade out (R..2R) and beyond, the far
/// ground takes, per azimuth bin, the ratio of the planes' lit ground to the far model's measured just inside R
/// (colour included), and a fine speckle of the planes' own texel contrast there, fading with distance. Measured
/// once when the planes are attached, from their texels and the map's light: bare lookups and maps without planes
/// are unchanged.
///
/// A road is two grounds, the asphalt and what lies beside it, and it bends: the ratio at the road's azimuths just
/// inside R is the asphalt's (and its paint's), which painted radially out to the horizon drew a straight road over
/// the bent far road. So where the far model
/// tells its own material apart (`material`: the road's share at a ground point), the asphalt takes one ratio over
/// the whole turn and the ground beside it the bins', each texel by its own share; a far point then takes the ratio
/// of what it is, wherever the road runs.
#[derive(Clone, Debug, PartialEq)]
pub struct FloorMatch {
    reach: f32,
    gain: Vec<Vec3f>,
    contrast: Vec<f32>,
    /// The second material's ratio over the whole turn (a road's asphalt), where the far model has one.
    second: Option<Vec3f>,
    /// A planes texel (radians): the speckle's scale.
    texel: f32,
    seed: u32,
}

impl FloorMatch {
    /// Measured over the planes' rows from 0.4 to 0.58 of the near reach `reach` (where the planes are wholly the
    /// near relief: their fade starts at 0.6 R at the earliest, `fade_start`): `near(d, texel)` is the planes' lit ground along d, `far(d)` the far model's. None when no
    /// texel lies in that band. `material(d)` is the far model's share of its second material at the ground point
    /// along d (a road's asphalt; None for a ground of one material), whose ratio is taken over the whole turn.
    pub fn measure(planes: &FloorPlanes, reach: f32, seed: u32, near: &dyn Fn(Vec3f, &FloorTexel) -> Vec3f, far: &dyn Fn(Vec3f) -> Vec3f, material: Option<&dyn Fn(Vec3f) -> f32>) -> Option<FloorMatch> {
        let eye = planes.eye_m();
        if !(reach > 0.0 && eye > 0.0) {
            return None;
        }
        let (w, h) = (planes.width(), planes.height());
        let row = 360.0 / w as f32;
        let (top, bottom) = ((eye / (0.58 * reach)).atan().to_degrees(), (eye / (0.4 * reach)).atan().to_degrees());
        let mut near_sum = vec![Vec3f::default(); MATCH_BINS];
        let mut far_sum = vec![Vec3f::default(); MATCH_BINS];
        let mut weight = vec![0.0f32; MATCH_BINS];
        let (mut near_second, mut far_second, mut weight_second) = (Vec3f::default(), Vec3f::default(), 0.0f32);
        let mut spread = vec![0.0f32; MATCH_BINS];
        let mut rows = vec![0.0f32; MATCH_BINS];
        for y in 0..h {
            let el = planes.top_deg() - (y as f32 + 0.5) * row;
            if -el < top || -el > bottom {
                continue;
            }
            // per bin within the row its mean and its spread (one depression: no trend with distance in it)
            let mut l1 = [0.0f32; MATCH_BINS];
            let mut l2 = [0.0f32; MATCH_BINS];
            let mut n = [0.0f32; MATCH_BINS];
            // (a bin's statistics need no more than about sixteen texels a row: wide planes are read in steps)
            for x in (0..w).step_by((w / 1024).max(1)) {
                let az = ((x as f32 + 0.5) / w as f32 - 0.5) * 360.0;
                let d = super::dir_from_az_el(az, el);
                let b = x * MATCH_BINS / w;
                let ln = near(d, &planes.texel(x, y));
                let lf = far(d);
                let m = material.map_or(0.0, |material| material(d).clamp(0.0, 1.0));
                near_sum[b] += ln * (1.0 - m);
                far_sum[b] += lf * (1.0 - m);
                weight[b] += 1.0 - m;
                near_second += ln * m;
                far_second += lf * m;
                weight_second += m;
                let l = crate::sky::luminance(ln);
                l1[b] += l;
                l2[b] += l * l;
                n[b] += 1.0;
            }
            for b in 0..MATCH_BINS {
                if n[b] > 0.0 && l1[b] > 0.0 {
                    let m = l1[b] / n[b];
                    spread[b] += (l2[b] / n[b] - m * m).max(0.0).sqrt() / m;
                    rows[b] += 1.0;
                }
            }
        }
        if !rows.iter().any(|&r| r > 0.0) {
            return None;
        }
        let ratio = |a: f32, b: f32| if b > 0.0 && a.is_finite() { (a / b).clamp(0.5, 2.0) } else { 1.0 };
        let ratio3 = |a: Vec3f, b: Vec3f| vec3f(ratio(a.x, b.x), ratio(a.y, b.y), ratio(a.z, b.z));
        // the ground beside the second material, over the whole turn: the bins the second material fills (fewer than
        // a texel's worth of the first in them) take it
        let whole = ratio3(near_sum.iter().fold(Vec3f::default(), |s, &v| s + v), far_sum.iter().fold(Vec3f::default(), |s, &v| s + v));
        let mut gain: Vec<Vec3f> = (0..MATCH_BINS).map(|b| if weight[b] >= 1.0 { ratio3(near_sum[b], far_sum[b]) } else { whole }).collect();
        let second = material.map(|_| if weight_second > 0.0 { ratio3(near_second, far_second) } else { whole });
        let mut contrast: Vec<f32> = (0..MATCH_BINS).map(|b| if rows[b] > 0.0 { (spread[b] / rows[b]).clamp(0.0, 0.3) } else { 0.0 }).collect();
        // smoothed round the turn (two passes of 1 2 1), so the bins never show as sectors
        for _ in 0..2 {
            let (g, c) = (gain.clone(), contrast.clone());
            for b in 0..MATCH_BINS {
                let (p, q) = ((b + MATCH_BINS - 1) % MATCH_BINS, (b + 1) % MATCH_BINS);
                gain[b] = (g[p] + g[b] * 2.0 + g[q]) * 0.25;
                contrast[b] = (c[p] + c[b] * 2.0 + c[q]) * 0.25;
            }
        }
        Some(FloorMatch { reach, gain, contrast, second, texel: std::f32::consts::TAU / w as f32, seed })
    }

    /// The near reach it hands over at (metres).
    pub fn reach(&self) -> f32 {
        self.reach
    }

    /// The contrast measured at the hand-over along `d` (the bins' mean of the planes' texel spread over their mean).
    pub fn contrast_at(&self, d: Vec3f) -> f32 {
        let u = 0.5 + d.x.atan2(-d.z) * (1.0 / std::f32::consts::TAU);
        let b = ((u * MATCH_BINS as f32) as i64).rem_euclid(MATCH_BINS as i64) as usize;
        self.contrast[b]
    }

    /// What the far ground along `d` (r metres away on the ground) is multiplied by: the bins' gain, read linearly
    /// between their centres (the second material's over the whole turn by its share `material` at the point), times
    /// the speckle (`residual`: the share of the measured contrast the far model's own texture does not already
    /// carry); and how much of that applies, 0 until the planes' fade starts (`fade_start`) rising to 1 at 2R, the
    /// GPU's `near_rd`. `texel` is the lookup's (radians): the speckle fades where its cells are finer than it.
    pub fn at(&self, d: Vec3f, r: f32, texel: f32, residual: f32, material: f32) -> (Vec3f, f32) {
        let share = smooth(self.reach * fade_start(d, self.seed), 2.0 * self.reach, r);
        if !(share > 0.0) {
            return (Vec3f::all(1.0), 0.0);
        }
        let u = 0.5 + d.x.atan2(-d.z) * (1.0 / std::f32::consts::TAU);
        let f = u * MATCH_BINS as f32 - 0.5;
        let b0 = f.floor();
        let t = f - b0;
        let b0 = (b0 as i64).rem_euclid(MATCH_BINS as i64) as usize;
        let b1 = (b0 + 1) % MATCH_BINS;
        let mut gain = self.gain[b0] * (1.0 - t) + self.gain[b1] * t;
        if let Some(second) = self.second {
            let m = material.clamp(0.0, 1.0);
            gain = gain * (1.0 - m) + second * m;
        }
        let contrast = (self.contrast[b0] * (1.0 - t) + self.contrast[b1] * t) * residual * (self.reach / r.max(self.reach)).sqrt();
        (gain * (1.0 + contrast * self.speckle(d, texel)).max(0.0), share)
    }

    /// The far model's fine texture along `d`: two octaves of value noise in the map's own angles (cells of 1 and
    /// 2.5 planes texels: the near relief's texture at the hand-over, the same at every bake width), scaled to a
    /// standard deviation of about 1, each fading where a lookup `texel` radians wide is wider than its cells.
    fn speckle(&self, d: Vec3f, texel: f32) -> f32 {
        let az = d.x.atan2(-d.z);
        let el = d.y.clamp(-1.0, 1.0).asin();
        let mut s = 0.0;
        for (k, &(cells, weight)) in SPECKLE_CELLS.iter().enumerate() {
            let cell = cells * self.texel;
            let keep = if texel > 0.0 { smooth(1.0, 2.0, cell / texel) } else { 1.0 };
            if keep > 0.0 {
                let seed = self.seed.wrapping_mul(0x85eb_ca6b) ^ (0x5bec_0000 + k as u32);
                // (2.2 times as tall as wide, as the GPU's: the texture's strokes are upright at any distance)
                s += keep * weight * (cell_noise(az / cell, el / (2.2 * cell), seed) - 0.5);
            }
        }
        s * (1.0 / SPECKLE_SD)
    }
}

/// What the GPU's planes are lit for (`Env::floor_light`, the app's `FloorKey`): the key light's direction in the
/// map's own frame (the sun while any of its disc is up, else the moon once it is the key, `Env`'s key rule; None
/// without one: the planes' key term is then 0), and whether the weather snows and a lake freezes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FloorLight {
    pub key_dir: Option<[f32; 3]>,
    pub snow: bool,
    pub frozen: bool,
}

/// The map's light as the floor's planes are composed with it (`Env`'s, measured once in its first phase): the
/// atmosphere's sky on a level ground and its sun at normal incidence, the night's ground light and the moon's part of
/// it, which of the two is the key, and the sky as drawn as L1 moments (its integral and its first moment over the
/// upper hemisphere: M0 and M), from which a tilted face's sky light is worked out.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FloorLighting {
    pub sky: Vec3f,
    pub sun: Vec3f,
    pub sun_level: f32,
    pub sun_key: bool,
    pub night: Vec3f,
    pub moon: Vec3f,
    /// The moon's light at normal incidence when it is the key.
    pub moon_key: Option<Vec3f>,
    pub sh: [Vec3f; 4],
}

impl FloorLighting {
    /// The light of `sh` on a level face (L1: M0 / 4 + My / 2), per channel, and its inverse where it is above 0
    /// (0 where it is not: `tilt` is then 1).
    fn level(&self) -> (Vec3f, Vec3f) {
        let [m0, _, my, _] = self.sh;
        let up = m0 * 0.25 + my * 0.5;
        let inv = |u: f32| if u > 0.0 { 1.0 / u } else { 0.0 };
        (up, vec3f(inv(up.x), inv(up.y), inv(up.z)))
    }
}

impl FloorLighting {
    /// The sky's light on a face of normal `n` over a level face's, per channel: L1's irradiance M0 / 4 + M.n / 2 over
    /// its value straight up (exact for a uniform sky over the upper hemisphere). 1 for a level face, bit for bit; 1
    /// where the sky puts no light.
    pub fn tilt(&self, n: [f32; 3]) -> Vec3f {
        // a level face: 1, bit for bit
        if n == [0.0, 1.0, 0.0] {
            return Vec3f::all(1.0);
        }
        let [m0, mx, my, mz] = self.sh;
        let (up, inv) = self.level();
        let at = m0 * 0.25 + (mx * n[0] + my * n[1] + mz * n[2]) * 0.5;
        let r = |a: f32, u: f32, i: f32| if u > 0.0 { (a * i).max(0.0) } else { 1.0 };
        vec3f(r(at.x, up.x, inv.x), r(at.y, up.y, inv.y), r(at.z, up.z, inv.z))
    }

    /// The day's light on a texel (irradiance, before the albedo): the sky's, tilted, and the sun's by the planes' key
    /// term when the sun is the key (else on a level ground, as the flat ground took it).
    pub fn day(&self, n: [f32; 3], key: f32) -> Vec3f {
        let k = if self.sun_key { key } else { self.sun_level };
        self.sky * self.tilt(n) + self.sun * k
    }

    /// The night's light on a texel (irradiance): its sky's, tilted, the moon's part by the planes' key term when the
    /// moon is the key.
    pub fn night(&self, n: [f32; 3], key: f32) -> Vec3f {
        match self.moon_key {
            Some(moon) => (self.night - self.moon) * self.tilt(n) + moon * key,
            None => self.night * self.tilt(n),
        }
    }

    /// The light a face of normal `n` takes from the ground round it (irradiance; day, night): the ground a level
    /// Lambertian plane of albedo `ground`, lit as level ground is (the sky and the sun on it, the night's light), so
    /// its radiance is ground x E / pi, which the face sees over the view factor (1 - n.y) / 2. `tilt`'s moments are
    /// the sky's upper hemisphere alone, so a tilted face took no light from the ground it faces: a slip face in the
    /// sun's shadow took the blue sky only. One bounce, exact for a level plane round the face; 0 for a level face.
    pub fn bounce(&self, n: [f32; 3], ground: Vec3f) -> (Vec3f, Vec3f) {
        let view = ((1.0 - n[1]) * 0.5).clamp(0.0, 1.0);
        if !(view > 0.0) {
            return (Vec3f::default(), Vec3f::default());
        }
        let g = ground * view;
        (g * (self.sky + self.sun * self.sun_level), g * self.night)
    }
}

/// The floor along one direction for the map (`Env::floor_at`): the far model there and, where the planes reach, their
/// texel and the share of the lookup they hold.
#[derive(Clone, Copy, Debug)]
pub struct FloorHere {
    pub far: FloorFar,
    pub near: Option<(FloorTexel, f32)>,
}

impl FloorHere {
    fn mixed(&self, far: f32, near: impl Fn(&FloorTexel) -> f32) -> f32 {
        match &self.near {
            Some((t, w)) => far + (near(t) - far) * *w,
            None => far,
        }
    }

    /// The share of the texel that is open water.
    pub fn water(&self) -> f32 {
        self.mixed(self.far.water, |t| t.water)
    }

    /// The share that is ice.
    pub fn ice(&self) -> f32 {
        self.mixed(self.far.ice, |t| t.ice)
    }

    /// How much of the atmosphere's blend into the horizon haze the ground keeps: the far model's, or over a valley
    /// the planes' distance (the Mountain's aerial perspective).
    pub fn keep(&self, kind: FloorKind) -> f32 {
        match kind {
            FloorKind::Mountain => self.mixed(self.far.keep, |t| if t.dist > 0.0 { (-t.dist / MOUNTAIN_HAZE_M).exp() } else { 1.0 }),
            _ => self.far.keep,
        }
    }

    /// How far the ground the texel shows lies, metres: the far model's, or the planes' own distance where they reach.
    /// A planes' texel at their distance's cap (PLANES_DIST_MAX_M) shows ground at least that far: the far model's
    /// ground beyond (a Mountain's valley floor 100 km out near the horizon) where that is farther.
    pub fn dist(&self) -> f32 {
        self.mixed(self.far.dist, |t| match t.dist {
            d if d >= PLANES_DIST_MAX_M => d.max(self.far.dist),
            d if d > 0.0 => d,
            _ => self.far.dist,
        })
    }
}

/// The share of a texel a wet ground puts under water by the floor's kind (plan hdri-8 §3, Q6), from `cover`, the
/// puddles' share where the ground's own hollows (the planes' hollow plane, or the weather's field over the floor's eye
/// beyond them) would hold them, at Wet ground `wet`: Grass holds none until the water rises between the blades
/// (`puddle_flood`, from 0.9); Sand only in its troughs from 0.8; Snow (slush) in its hollows from 0.6; the rest
/// (stones, ruts, joints, the summit's rock, the lake's shore) in their hollows from the start.
pub fn wet_cover(kind: FloorKind, wet: f32, cover: f32) -> f32 {
    let flood = super::weather::puddle_flood(wet);
    let held = match kind {
        FloorKind::Grass => 0.0,
        FloorKind::Sand => cover * smooth(0.8, 0.9, wet),
        FloorKind::Snow => cover * smooth(0.6, 0.7, wet),
        _ => cover,
    };
    if flood > 0.0 { 1.0 - (1.0 - held) * (1.0 - flood) } else { held }
}

/// The slopes (variance per axis) of the film of water on a wet ground between its puddles (`Env::damp_sheen`), by
/// what its surface is: the Road's asphalt and the Plaza's stones paved (weather::DAMP_SLOPES), Sand and Snow (slush)
/// granular, as the plain ground's sand and soil without a floor (DAMP_SLOPES_GRAINS), Rock, a Mountain's scree and a
/// Lake's shore stony (DAMP_SLOPES_STONES), Grass its blades (DAMP_SLOPES_BLADES).
pub(crate) fn damp_slopes(kind: Option<FloorKind>) -> f32 {
    use super::weather::{DAMP_SLOPES, DAMP_SLOPES_BLADES, DAMP_SLOPES_GRAINS, DAMP_SLOPES_STONES};
    match kind {
        Some(FloorKind::Road | FloorKind::Plaza) => DAMP_SLOPES,
        Some(FloorKind::Sand | FloorKind::Snow) | None => DAMP_SLOPES_GRAINS,
        Some(FloorKind::Rock | FloorKind::Mountain | FloorKind::Lake) => DAMP_SLOPES_STONES,
        Some(FloorKind::Grass) => DAMP_SLOPES_BLADES,
    }
}

/// The damp films of a floor's main, second and third grounds (`FloorModel::films`, as `damp_slopes` gives each
/// ground's): a Road's asphalt, its verge (a gravel shoulder: stony) and what lies beside it (the Grass field's blades,
/// Gravel's stones, Sand's grains, Concrete and a room's paving paved); a Lake's beach (its shore's, stony) and its far
/// bank (half soil, granular, half the Grass field: their mixed variance); the other kinds' one ground throughout.
fn damp_films(floor: &FloorParams) -> [f32; 3] {
    use super::weather::{DAMP_SLOPES, DAMP_SLOPES_BLADES, DAMP_SLOPES_GRAINS, DAMP_SLOPES_STONES};
    let kind = floor.kind();
    let main = damp_slopes(Some(kind));
    match kind {
        FloorKind::Road => {
            let beside = match floor.beside() {
                RoadBeside::Grass => DAMP_SLOPES_BLADES,
                RoadBeside::Gravel => DAMP_SLOPES_STONES,
                RoadBeside::Sand => DAMP_SLOPES_GRAINS,
                RoadBeside::Concrete | RoadBeside::Room => DAMP_SLOPES,
            };
            [main, DAMP_SLOPES_STONES, beside]
        }
        FloorKind::Lake => [main, main, 0.5 * (DAMP_SLOPES_GRAINS + DAMP_SLOPES_BLADES)],
        _ => [main; 3],
    }
}

/// The puddles' share of a texel from its hollow (0..1) at Wet ground `wet`: the deepest hollows first, more of them
/// the wetter it is (a hollow of 1 holds water from the first drop; at Wet 1, every hollow over 0.1).
pub fn hollow_cover(hollow: f32, wet: f32) -> f32 {
    let threshold = 1.0 - 0.9 * wet.clamp(0.0, 1.0);
    smooth(threshold - 0.05, threshold + 0.05, hollow)
}

/// The analytic glitter of a light on rippled water (§3.8: not sampled, so it never sparkles from texel to texel):
/// Cox and Munk's facets, a Gaussian of slopes whose variance per axis is `slopes` (the ripples' the texel leaves
/// unresolved) plus the light's own disc (`radius`, radians) and the texel's spread (`texel`, radians), centred on the
/// slope of `n`, the resolved ripples' normal (straight up: all the ripples in `slopes`). The lobe's geometry is the
/// level surface's: the facets' slopes are offset by the resolved ones, not the surface turned, so over the texels the
/// resolved slopes sample, the light is the whole lobe's whatever share is resolved. `v` is toward the eye, `l` toward
/// the light, `irradiance` the light's at normal incidence. Its light over the view directions is the mirror's,
/// Fresnel's share of the light, less what the facets hide from each other near the horizon (Smith's masking and
/// shadowing by `all`, the variance of all the ripples' slopes).
pub fn glitter(v: Vec3f, l: Vec3f, n: Vec3f, irradiance: Vec3f, radius: f32, slopes: f32, all: f32, texel: f32, f0: f32) -> Vec3f {
    let h = (v + l).normalize();
    let (ch, cv) = (h.y, v.y);
    if !(ch > 0.0 && cv > 0.0 && l.y > 0.0 && n.y > 0.0) {
        return Vec3f::default();
    }
    // (the texel: a reflected lobe 2 sigma wide spans a texel and a half, so a bake samples it, never misses it)
    let s2 = slopes + 0.125 * radius * radius + 0.5 * texel * texel;
    if !(s2 > 0.0) {
        return Vec3f::default();
    }
    // the facet slope the light needs (the half vector's) less the resolved slope there
    let (dx, dz) = (h.x / ch - n.x / n.y, h.z / ch - n.z / n.y);
    let tan2 = dx * dx + dz * dz;
    // past 8 sigma the lobe is nothing (e^-32)
    if tan2 > 64.0 * s2 {
        return Vec3f::default();
    }
    let p = (-0.5 * tan2 / s2).exp() / (std::f32::consts::TAU * s2);
    let vh = v.dot(h).clamp(0.0, 1.0);
    let fresnel = f0 + (1.0 - f0) * (1.0 - vh).powi(5);
    // the facets' masking and shadowing (Smith's, of all the ripples' slopes `all`): near the horizon the ripples hide
    // each other, so the glitter fades there instead of growing as 1 / cv
    let masked = |c: f32| 2.0 / (1.0 + (1.0 + 2.0 * all * (1.0 - c * c).max(0.0) / (c * c)).sqrt());
    irradiance * (fresnel * p * masked(cv) * masked(l.y) / (4.0 * cv * ch * ch * ch * ch))
}

/// What lies beside a road (`FloorParams::beside`), its albedo under the floor's `tint`; None for Grass (the field:
/// the GPU's verge and field draw it). Gravel the Rock kind's gravel, Sand the Sand kind's, Concrete a mid grey
/// paving, Room the colour the room standing on the ground gives it (`beside_color`, untinted: it is the room's own).
fn road_beside(floor: &FloorParams, tint: Vec3f) -> Option<Vec3f> {
    match floor.beside() {
        RoadBeside::Grass => None,
        RoadBeside::Gravel => Some(v3(FloorModel::recipe(FloorKind::Rock).alt) * tint),
        RoadBeside::Sand => Some(v3(FloorModel::recipe(FloorKind::Sand).albedo) * tint),
        RoadBeside::Concrete => Some(v3(CONCRETE_ALBEDO) * tint),
        RoadBeside::Room => Some(v3(floor.beside_color.unwrap_or(CONCRETE_ALBEDO))),
    }
}

/// The road's centre line on the ground (plan §3.5; bends 2026-11-07): a straight along Heading through the
/// viewer's place, or one that bends ahead: straight to where the bend starts, then a circular arc of the bend's
/// radius through a quarter turn, then straight on along the new way. With the start at or behind the viewer the
/// viewer stands on the arc, which began that far behind (never more than a quarter turn behind), the road's
/// direction at the viewer still the Heading, so a room standing on the road (a tunnel, a bridge) keeps the road
/// between its mouths. Both the far model and the GPU's relief (`makepad_floor`'s road shader, the same pieces) read
/// a ground point through `frame`: how far it lies across the road (signed, + to the left of the way ahead) and
/// along it, and the road's direction there, so the lanes, their markings and the ruts follow the bend.
#[derive(Clone, Copy, Debug)]
pub struct RoadPath {
    /// The heading (unit, x and z: a light's azimuth, 0 north) and the lateral unit (+: left of it).
    a: [f32; 2],
    n: [f32; 2],
    /// The arc's radius (metres) and which side its centre lies (+1 left, -1 right; 0: straight).
    r: f32,
    sgn: f32,
    /// The arc's centre, the unit from it to the anchor (where the road's direction is the heading), the heading
    /// there, the arc's angles from the anchor (radians, `lo` 0 or less, `hi` the quarter turn) and the anchor's
    /// distance along the road.
    c: [f32; 2],
    e1: [f32; 2],
    e2: [f32; 2],
    lo: f32,
    hi: f32,
    u0: f32,
}

/// A ground point in the road's frame (`RoadPath::frame`): across the road (`v`, metres, + to the left of the way
/// ahead), along it (`u`, metres, + ahead) and the road's direction there (`t`, unit, x and z).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoadFrame {
    pub v: f32,
    pub u: f32,
    pub t: [f32; 2],
}

/// The bend's sweep: a quarter turn, then straight on.
const ROAD_SWEEP: f32 = std::f32::consts::FRAC_PI_2;

impl RoadPath {
    /// A road along `heading_deg`, bending with `signed_radius` (`FloorParams::signed_curve_radius`: 0 straight,
    /// negative left, positive right) from `start_m` ahead (0 or less: the viewer stands on the bend).
    pub fn new(heading_deg: f32, signed_radius: f32, start_m: f32) -> RoadPath {
        let h = heading_deg.to_radians();
        // a light's azimuth: 0 north (-z), 90 east (+x); the lateral unit has the road's +v side on its left
        let a = [h.sin(), -h.cos()];
        let n = [a[1], -a[0]];
        let r = signed_radius.abs();
        let sgn = if !(r >= 1.0) || !signed_radius.is_finite() { 0.0 } else if signed_radius < 0.0 { 1.0 } else { -1.0 };
        let r = if sgn == 0.0 { 0.0 } else { r };
        let start = if start_m.is_finite() { start_m } else { 0.0 };
        let u0 = start.max(0.0);
        let lo = if sgn == 0.0 { 0.0 } else { (start.min(0.0) / r).max(-ROAD_SWEEP) };
        let anchor = [a[0] * u0, a[1] * u0];
        let c = [anchor[0] + sgn * r * n[0], anchor[1] + sgn * r * n[1]];
        let e1 = [-sgn * n[0], -sgn * n[1]];
        RoadPath { a, n, r, sgn, c, e1, e2: a, lo, hi: ROAD_SWEEP, u0 }
    }

    /// Whether the road bends.
    pub fn bends(&self) -> bool {
        self.sgn != 0.0
    }

    /// The arc's point, outward radial and direction at angle `phi` from the anchor.
    fn arc(&self, phi: f32) -> ([f32; 2], [f32; 2], [f32; 2]) {
        let (sn, cs) = phi.sin_cos();
        let rad = [self.e1[0] * cs + self.e2[0] * sn, self.e1[1] * cs + self.e2[1] * sn];
        let t = [-self.e1[0] * sn + self.e2[0] * cs, -self.e1[1] * sn + self.e2[1] * cs];
        ([self.c[0] + self.r * rad[0], self.c[1] + self.r * rad[1]], rad, t)
    }

    /// The ground point (x, z) in the road's frame.
    pub fn frame(&self, x: f32, z: f32) -> RoadFrame {
        if self.sgn == 0.0 {
            return RoadFrame { v: x * self.a[1] - z * self.a[0], u: x * self.a[0] + z * self.a[1], t: self.a };
        }
        let q = [x - self.c[0], z - self.c[1]];
        let theta = (q[0] * self.e2[0] + q[1] * self.e2[1]).atan2(q[0] * self.e1[0] + q[1] * self.e1[1]);
        if theta >= self.lo && theta <= self.hi {
            let rho = (q[0] * q[0] + q[1] * q[1]).sqrt();
            let (_, _, t) = self.arc(theta);
            return RoadFrame { v: -self.sgn * (rho - self.r), u: self.u0 + self.r * theta, t };
        }
        let phi = if theta < self.lo { self.lo } else { self.hi };
        let (s, rad, t) = self.arc(phi);
        let w = [x - s[0], z - s[1]];
        RoadFrame { v: -self.sgn * (w[0] * rad[0] + w[1] * rad[1]), u: self.u0 + self.r * phi + w[0] * t[0] + w[1] * t[1], t }
    }

    /// The point `u` metres along the road and the road's direction there.
    pub fn point(&self, u: f32) -> ([f32; 2], [f32; 2]) {
        if self.sgn == 0.0 {
            return ([self.a[0] * u, self.a[1] * u], self.a);
        }
        let theta = (u - self.u0) / self.r;
        if theta >= self.lo && theta <= self.hi {
            let (p, _, t) = self.arc(theta);
            return (p, t);
        }
        let phi = if theta < self.lo { self.lo } else { self.hi };
        let (s, _, t) = self.arc(phi);
        let along = u - (self.u0 + self.r * phi);
        ([s[0] + t[0] * along, s[1] + t[1] * along], t)
    }

    /// The lateral unit at direction `t`: the road's left.
    pub fn left_of(t: [f32; 2]) -> [f32; 2] {
        [t[1], -t[0]]
    }

    /// The heading's lateral unit (the road's left at the viewer).
    pub fn left(&self) -> [f32; 2] {
        self.n
    }
}

/// The night air's visibility in a clear sky's haze (1): the distance over which the ground's light falls to 1/e on
/// its way to the viewer (`FloorModel::night_keep`); a clear night's meteorological range.
const NIGHT_VISIBILITY_M: f32 = 20_000.0;

/// A mid grey concrete paving's albedo (linear): what lies beside a road set to Concrete.
const CONCRETE_ALBEDO: [f32; 3] = [0.36, 0.35, 0.33];

/// How far the ground beside a road creeps over the asphalt's edge on average at Overgrowth 1, metres (the GPU's
/// `og_creep`).
const OVERGROWTH_CREEP_M: f32 = 0.225;
/// The overgrowth's moss and fine grass along the cracks: their mean albedo green and at dryness 1 (the GPU's `mo_col`,
/// mixed by the dryness squared).
const OVERGROWTH_MOSS: [[f32; 3]; 2] = [[0.07, 0.118, 0.031], [0.11, 0.127, 0.049]];
/// The moss's brightness against those greens (the GPU's `mo_col` scales its colour the same): the moss references'
/// (D:/hdri-refs, luminance about 0.04-0.07 against the greens' 0.10). The owner chose it over the full greens, which
/// read as lime paint in the joints at noon; at a 7 degree sun the road's cracks read darker, still green.
const OVERGROWTH_MOSS_SHADE: f32 = 0.52;
/// A road's cracks' length a square metre over Wear squared (along the tracks, across, the bays' secondary and tertiary
/// ones, as `cracks` lays them; measured against the near relief's mean; the fatigue mesh is `road_fatigue`'s).
const ROAD_CRACKS_M: f32 = 2.2;
/// The Rock floor's slab's share of its ground (the GPU's `smoothstep(0.52, 0.66, ..)` of a gradient noise).
const ROCK_SLAB_SHARE: f32 = 0.25;
/// The moss's cushions' mean cover over its band (the GPU's `mo_take` under the footprint: its resolved cushions'
/// mean over a band's profile).
const MOSS_CUSHIONS: f32 = 0.83;

/// A road's fatigue cracking at Wear `wr` on lanes `lane` m wide (the GPU's `gshare`, `gator`): the share of the road
/// it has cracked (two bands of about 0.6 m a lane, the share of their length over its threshold, as `crack_mean`) and
/// the length a square metre of road of the mesh's lines the moss lines (its long lines, its cracks across and its
/// chords: 4.5, 2.5 and 1.2 m a square metre of the mesh, the chords by Wear).
fn road_fatigue(wr: f32, lane: f32) -> (f32, f32) {
    if !(wr > 0.6) {
        return (0.0, 0.0);
    }
    let th = 1.0 - 1.7333 * (wr - 0.6);
    let share = 1.2 / lane.max(0.5) * (0.98 - 1.2 * (th - 0.025)).clamp(0.0, 1.0);
    (share, share * (4.5 + 2.5 + 1.2 * ((wr - 0.6) / 0.3).clamp(0.0, 1.0)))
}

/// The share of a Plaza's ground the moss's band takes at reach `reach` (m) on paving of `pattern`, tile `tile` m, its
/// joints `joint` of the ground (the GPU's `og_bmean`): the joints and a rim of the reach inside every unit, the rims'
/// overlap at the units' corners taken in, with L the joints' half width (the GPU's `jh`) plus the reach, a square tile
/// or slab 1 - (1 - 2 L / s)^2, a brick 1 - (1 - 2 L / s)(1 - 4 L / s), a fan's sett its rounded square inset by L; a
/// sett 0.8 k^2 of its cell, its rim along the GPU's first order distance: t^2 of it kept, t^4 + b t^3 = 1, b = 8.14
/// reach / (k s).
fn plaza_band(pattern: FloorPattern, tile: f32, joint: f32, reach: f32) -> f32 {
    let (s, c) = (tile, joint);
    if pattern == FloorPattern::Cobbles {
        let k = ((1.0 - c) / 0.8).sqrt().min(0.97);
        let b = 8.14 * reach / (k * s);
        let mut t = 1.0 / (1.0 + 0.25 * b);
        for _ in 0..2 {
            t -= (t * t * t * (t + b) - 1.0) / (t * t * (4.0 * t + 3.0 * b));
        }
        return 1.0 - 0.8 * k * k * t * t;
    }
    let jh = match pattern {
        FloorPattern::Herringbone => 0.5 * s * (3.0 - (9.0 - 8.0 * c).sqrt()) / 4.0,
        FloorPattern::Fan => {
            let q = 0.25 * s.min(0.2) / s;
            let b = 4.0 - 1.7168147 * q;
            s * (b - (b * b - 12.566371 * (c - 0.8584073 * q * q)).sqrt()) / 6.2831853
        }
        _ => 0.5 * s * (1.0 - (1.0 - c).sqrt()),
    };
    let u = (jh + reach) / s;
    let a = (1.0 - 2.0 * u).max(0.0);
    match pattern {
        FloorPattern::Herringbone => 1.0 - a * (1.0 - 4.0 * u).max(0.0),
        FloorPattern::Fan => {
            let qr = (0.25 * s.min(0.2) / s - u).max(0.0);
            1.0 - (a * a - 0.8584073 * qr * qr).max(0.0)
        }
        _ => 1.0 - a * a,
    }
}

/// The share of the cracks' courses the moss and fine grass have taken where `mo_take` is given the share `sh` (the
/// GPU's `mo_patch`, its mean: its threshold 0.76 - 0.52 sh over a noise held as `smoothstep(th - 0.26, th + 0.26, ..)`).
fn moss_patch(sh: f32) -> f32 {
    let x = sh.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// The overgrowth's plants' mean albedo, untinted (the GPU's `og_pmean`): a clump's blades (greener and darker on a
/// weed, paler on the dune grass, by how `upright` they stand) and their straw by the dryness `dry`, a rosette's leaves,
/// by the share of rosettes `rshare`.
fn overgrowth_plants(rshare: f32, upright: f32, dry: f32) -> Vec3f {
    let blades = v3([0.058, 0.11, 0.03]).mix(v3([0.13, 0.15, 0.085]), upright).mix(v3([0.3, 0.26, 0.13]), 0.5 * dry * dry + 0.15 * dry);
    blades.mix(v3([0.047, 0.075, 0.029]), rshare)
}

/// The overgrowth's plants' mean cover seen `s` (the sine of the depression) down, at Overgrowth `og`, for a kind whose
/// plants stand one in a cell `cell` m across by Overgrowth's chance times the kind's mean say `say`, the largest
/// `size` (a rosette's radius, a clump's height, m) at Overgrowth 1, a share `rshare` of them rosettes, the clumps'
/// blades `upright` (the GPU's `og_tau`, `og_mean`): n a square metre; a rosette 0.675 of the largest radius on
/// average, its leaves 0.6 of its disc, a third as high as wide; a clump 0.7 of the tallest high, its blades' cover
/// from above and from the side by that height (the GPU's `og_cs`: 0.0032 and 0.0064 m^2 a metre for a weed's,
/// 0.005 and 0.0205 for the dune grass's).
fn overgrowth_cover(cell: f32, size: [f32; 2], rshare: f32, upright: f32, say: f32, og: f32, s: f32) -> f32 {
    let n = og * say / (cell * cell);
    let rr = 0.675 * size[0] * (0.6 + 0.4 * og);
    let hc = 0.7 * size[1] * (0.5 + 0.5 * og);
    let pi = std::f32::consts::PI;
    let mx = |a: f32, b: f32| a + (b - a) * upright;
    let top = rshare * 0.6 * pi * rr * rr + (1.0 - rshare) * mx(0.0032, 0.005) * hc;
    let side = rshare * 0.33 * rr * rr + (1.0 - rshare) * mx(0.0064, 0.0205) * hc;
    let cot = (1.0 - s * s).max(0.0).sqrt() / s.max(1.0e-6);
    1.0 - (-n * (top + side * cot)).exp()
}

/// A Grass field's mean albedo seen `s` (the sine of the depression) down (§3.1): green running to golden by
/// Dryness, bare soil in patches that grow with Dryness (seen from any angle) and between the blades (seen only
/// looking down: at grazing angles only blades show).
fn grass_mean(recipe: &FloorRecipe, dryness: f32, s: f32) -> Vec3f {
    // (the hue turns golden late: a field at Dryness 0.2 is still green)
    let blades = v3(recipe.albedo).mix(v3(recipe.alt), dryness * dryness);
    let soil = 0.35 * smooth(0.5, 1.0, dryness) + 0.15 * s;
    blades.mix(v3(recipe.under), soil)
}

/// How much of a Grass field the weather's 10 cm of snow covers: all of short grass, the tips of long grass poking out.
fn grass_snow(length_m: f32) -> f32 {
    1.0 - 0.35 * smooth(0.10, 0.6, length_m)
}


#[cfg(test)]
pub(crate) mod tests {
    use super::super::*;
    use super::{v3, FloorModel, SeaEdge, FLOOR_FAR_SHORE_RANGE, SNOW_ALBEDO};

    /// Runs every index on four scoped threads, interleaved, as fan_out would.
    fn four_threads(n: usize, f: &(dyn Fn(usize) + Sync)) {
        std::thread::scope(|scope| {
            for t in 0..4 {
                scope.spawn(move || {
                    let mut i = t;
                    while i < n {
                        f(i);
                        i += 4;
                    }
                });
            }
        });
    }

    /// A synthetic strip: a dark city 0 to 6 degrees up with lit windows, a near rise and its flat ground under the
    /// horizon, distances by row, at the strip's eye.
    fn synthetic_band() -> std::sync::Arc<skyline::SkylineBand> {
        let (w, h, top, bottom) = (96usize, 24usize, 8.0f32, -4.0f32);
        let row = (top - bottom) / h as f32;
        let mut texels = Vec::with_capacity(w * h);
        let mut dist = Vec::with_capacity(w * h);
        for y in 0..h {
            let el = top - (y as f32 + 0.5) * row;
            for x in 0..w {
                let tall = 2.0 + 4.0 * ((x * 7 % 11) as f32 / 10.0);
                let (t, far) = if el > tall {
                    ([0.0; 4], 0.0)
                } else if el > 0.0 {
                    let lit = if (x + y) % 5 == 0 { 0.6 } else { 0.02 };
                    ([lit, lit * 0.8, lit * 0.5, 1.0], 1500.0 + 40.0 * x as f32)
                } else {
                    let floor = (-el / 2.0).min(1.0);
                    ([0.03 * (1.0 - floor), 0.03 * (1.0 - floor), 0.02 * (1.0 - floor), 1.0 - floor], 200.0)
                };
                texels.push(t);
                dist.push(far);
            }
        }
        std::sync::Arc::new(skyline::SkylineBand::new(w, h, texels, dist, top, bottom).with_eye(6.3))
    }

    /// The maps the floor block is checked on (plan hdri-8 §6): Clear noon, Sunset, Overcast; rain with Wet ground 0.7; the
    /// Snow kind; the moonlit night under rain; a studio window filled with sky; Blue hour under rain (Wet ground 0.5)
    /// with a synthetic skyline strip attached.
    pub(crate) fn floor_baseline_cases() -> Vec<(&'static str, HdriParams, ImageSet)> {
        use super::super::presets::preset;
        let plain = ImageSet::default();
        let weather = |kind: &str, wet: f32| Some(WeatherParams { kind: kind.to_string(), wet_ground: wet, ..WeatherParams::default() });
        let noon = preset("Clear noon").unwrap();
        let mut rain = noon.clone();
        rain.sky.weather = weather("rain", 0.7);
        let mut snow = noon.clone();
        snow.sky.weather = weather("snow", 0.0);
        let mut moonlit_rain = preset("Moonlit night").unwrap();
        moonlit_rain.sky.weather = weather("rain", 0.0);
        moonlit_rain.sky.clouds.coverage = 0.5;
        let window = LightParams { gobo: Some(GoboParams { panes_x: 1, panes_y: 1, frame: 0.0, fill_sky: true, ..GoboParams::default() }), ..LightParams::default() };
        let studio = HdriParams { mode: "studio".to_string(), lights: vec![window], ..noon.clone() };
        let mut city = preset("Blue hour").unwrap();
        city.sky.weather = weather("rain", 0.5);
        city.skyline = Some(SkylineParams { on: true, seed: 7, turn_deg: 20.0, lights_by_day: 0.0, settings: Default::default() });
        let with_band = ImageSet { skyline: Some(synthetic_band()), ..ImageSet::default() };
        vec![
            ("clear_noon", noon, plain.clone()),
            ("sunset", preset("Sunset").unwrap(), plain.clone()),
            ("overcast", preset("Overcast").unwrap(), plain.clone()),
            ("rain_wet", rain, plain.clone()),
            ("snow", snow, plain.clone()),
            ("moonlit_rain", moonlit_rain, plain.clone()),
            ("studio_window", studio, plain),
            ("city_band", city, with_band),
        ]
    }

    // ---- plan hdri-8 T1: the floor block and the far model ---------------------------------------------------------

    fn floor_of(kind: &str) -> FloorParams {
        FloorParams { kind: kind.to_string(), ..FloorParams::default() }
    }

    fn with_floor(p: &HdriParams, floor: Option<FloorParams>) -> HdriParams {
        HdriParams { floor, ..p.clone() }
    }

    /// Two maps that must draw alike: every bake (serial, parallel, an export width), the key, the keyless copy and
    /// the strip's light are equal, bit for bit.
    fn same_maps(a: &HdriParams, b: &HdriParams, images: &ImageSet, what: &str) {
        let (ea, eb) = (Env::new_with(a, images), Env::new_with(b, images));
        let (ma, mb) = (ea.bake(128), eb.bake(128));
        assert!(ma == mb, "{what}: the 128 bake");
        assert!(eb.bake_par(128, four_threads) == mb, "{what}: the parallel bake");
        assert_eq!(ea.sun(), eb.sun(), "{what}: the key");
        assert!(ea.bake_keyless_par(&ma, four_threads) == eb.bake_keyless_par(&mb, four_threads), "{what}: the keyless copy");
        assert!(ea.skyline_light() == eb.skyline_light(), "{what}: the strip's light");
        assert!(ea.bake(512) == eb.bake(512), "{what}: an export width");
    }

    /// Plan hdri-8 §6: a floor block that is off, a floor in Studio mode and a floor under an image that is the whole
    /// base draw nothing: every bake, the key, the keyless copy and the strip's light are the floorless map's.
    #[test]
    fn floor_block_changes_nothing() {
        let off = FloorParams { on: false, ..floor_of("lake") };
        for (name, p, images) in floor_baseline_cases() {
            same_maps(&p, &with_floor(&p, Some(off.clone())), &images, &format!("{name}, a block that is off"));
        }
        let cases = floor_baseline_cases();
        let studio = &cases.iter().find(|(name, _, _)| *name == "studio_window").unwrap().1;
        for kind in FLOOR_KIND_NAMES {
            same_maps(studio, &with_floor(studio, Some(floor_of(kind))), &ImageSet::default(), &format!("Studio mode, {kind}"));
        }
        let mut images = ImageSet::default();
        images.images.insert("photo".to_string(), std::sync::Arc::new(EnvMap::from_fn(64, |d| if d[1] > 0.0 { [0.8, 0.9, 1.2] } else { [0.2, 0.2, 0.15] })));
        let mut photo = super::super::presets::preset("Clear noon").unwrap();
        photo.base_image = Some(BaseImageParams { key: "photo".to_string(), ..BaseImageParams::default() });
        photo.sky.weather = Some(WeatherParams { kind: "rain".to_string(), wet_ground: 0.6, ..WeatherParams::default() });
        for kind in FLOOR_KIND_NAMES {
            same_maps(&photo, &with_floor(&photo, Some(floor_of(kind))), &images, &format!("the whole base an image, {kind}"));
        }
    }

    /// Plan hdri-8 §6: a floor of one plain albedo is the old ground of that colour. A Snow floor (one albedo, 0.85,
    /// at every depression) tinted to the map's ground colour, its distant map off, lights the ground as the floorless
    /// map lights `ground_color`: by day, under an overcast, by night and under rain on a wet ground, within 1e-5.
    #[test]
    fn a_flat_floor_far_model_is_the_old_ground() {
        use super::super::presets::preset;
        let mut wet = preset("Clear noon").unwrap();
        wet.sky.weather = Some(WeatherParams { kind: "rain".to_string(), wet_ground: 0.7, ..WeatherParams::default() });
        let mut state = 7u32;
        let mut next = move || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 8) as f32 / 16_777_216.0
        };
        for (name, p) in [("noon", preset("Clear noon").unwrap()), ("overcast", preset("Overcast").unwrap()), ("night", preset("Moonlit night").unwrap()), ("rain on wet ground", wet)] {
            let g = p.sky.atmosphere.ground_color;
            let recipe = FloorModel::recipe(FloorKind::Snow).albedo;
            // (at the weather's own eye, 2 m, the puddles lie where the floorless map's do)
            let flat = FloorParams { tint: [g[0] / recipe[0], g[1] / recipe[1], g[2] / recipe[2]], eye_m: 2.0, ..floor_of("snow") };
            let old = Env::new(&p);
            let mut new = Env::new(&with_floor(&p, Some(flat)));
            new.floor = new.floor.take().map(|floor| floor.without_variation());
            let mut worst = 0.0f32;
            for _ in 0..400 {
                // by night the floor's ground runs on through the night air (`night_keep`) where the old ground
                // fades out over the 2 degrees under the horizon: the same below that band, to the air's share
                let el = if name == "night" { -2.0 - 87.9 * next() } else { -0.05 - 89.9 * next() };
                let d = dir_from_az_el(360.0 * next(), el);
                for texel in [0.0, std::f32::consts::TAU / 1024.0] {
                    let at = Sampling { texel, ..Sampling::default() };
                    let (a, b) = (old.radiance_with(d, None, at), new.radiance_with(d, None, at));
                    let scale = a.max_elem().max(1.0e-6);
                    worst = worst.max((a - b).abs().max_elem() / scale);
                }
            }
            println!("{name}: the flat floor is the old ground within {worst:e}");
            assert!(worst <= if name == "night" { 1.0e-2 } else { 1.0e-5 }, "{name}: {worst}");
        }
    }

    /// The mean luminance of the lower hemisphere of a bake (each row by its solid angle).
    fn ground_luminance(map: &EnvMap) -> f32 {
        let (w, h) = (map.width, map.height);
        let (mut sum, mut weight) = (0.0f64, 0.0f64);
        for y in h / 2..h {
            let el = (0.5 - (y as f32 + 0.5) / h as f32) * std::f32::consts::PI;
            let cos = el.cos() as f64;
            for x in 0..w {
                let t = map.data[y * w + x];
                sum += crate::sky::luminance(vec3f(t[0], t[1], t[2])) as f64 * cos;
                weight += cos;
            }
        }
        (sum / weight) as f32
    }

    /// Plan hdri-8 T1: every kind bakes a finite map, by day, at sunset, at night and under snow weather, and none
    /// lights its lower hemisphere more than the Snow floor does. The one exception is the Lake's open water under a
    /// low sun: a mirror at grazing angles, it shows the sun's glint and the bright sky by the sun, which outshine any
    /// diffuse ground lit by that sun (the floorless map's wet ground does the same); it is held to finite there.
    #[test]
    fn every_kind_bakes_a_finite_map_no_brighter_than_snow() {
        use super::super::presets::preset;
        let mut snowing = preset("Clear noon").unwrap();
        snowing.sky.weather = Some(WeatherParams { kind: "snow".to_string(), ..WeatherParams::default() });
        for (name, p) in [("noon", preset("Clear noon").unwrap()), ("sunset", preset("Sunset").unwrap()), ("night", preset("Moonlit night").unwrap()), ("snowing", snowing)] {
            let snow = ground_luminance(&Env::new(&with_floor(&p, Some(floor_of("snow")))).bake(128));
            for kind in FLOOR_KIND_NAMES {
                let map = Env::new(&with_floor(&p, Some(floor_of(kind)))).bake(128);
                assert!(map.data.iter().flatten().all(|v| v.is_finite()), "{name}, {kind}: a texel that is not finite");
                let lum = ground_luminance(&map);
                println!("{name}, {kind}: the ground's mean luminance {lum:.4} (snow {snow:.4})");
                assert!(lum > 0.0, "{name}, {kind}: dark");
                if !(*kind == "lake" && name == "sunset") {
                    assert!(lum <= snow * 1.001, "{name}, {kind}: {lum} against snow's {snow}");
                }
            }
        }
    }

    /// The RMS of the second differences of the far model's luminance along rows and down columns of a bake
    /// `width` wide, over its rows from `top` to `bottom` degrees down and its columns from azimuth `az.0` to `az.1`,
    /// relative to their mean: how much the far model changes from texel to texel faster than two texels.
    fn texel_noise(model: &FloorModel, width: usize, top: f32, bottom: f32, az: (f32, f32), texel: f32) -> f32 {
        let h = width / 2;
        let row_of = |el: f32| ((0.5 - el / 180.0) * h as f32) as usize;
        let lum = |x: usize, y: usize| {
            let el = 90.0 - (y as f32 + 0.5) * 180.0 / h as f32;
            let az = (x as f32 + 0.5) * 360.0 / width as f32;
            crate::sky::luminance(model.sample(dir_from_az_el(az, el), texel).albedo)
        };
        let (mut sum2, mut mean, mut n) = (0.0f64, 0.0f64, 0usize);
        for y in row_of(-top).max(h / 2 + 1)..=row_of(-bottom) {
            let column = |a: f32| (a / 360.0 * width as f32) as usize;
            for x in (column(az.0).max(1)..column(az.1).min(width - 1)).step_by(3) {
                let c = lum(x, y);
                let across = lum(x - 1, y) - 2.0 * c + lum(x + 1, y);
                let down = lum(x, y - 1) - 2.0 * c + lum(x, y + 1);
                sum2 += (across * across + down * down) as f64 * 0.5;
                mean += c as f64;
                n += 1;
            }
        }
        ((sum2 / n as f64).sqrt() / (mean / n as f64)) as f32
    }

    /// Plan hdri-8 §3.9: the far model is band-limited to the bake's texel. Near the horizon of a 1K bake (0.3 to 8
    /// degrees down, where a texel spans metres to kilometres of ground) the distant map's second differences from
    /// texel to texel stay under 1 % of its mean for every kind; the same model read along exact directions (texel 0,
    /// every octave whole) is several times noisier there. Each kind is read where its ground has no edge of its own
    /// (a road's, a shore's, the summit's), which are real lines the far model softens over a texel: the field beside
    /// the road, the lake's far bank behind the viewer past its beach, the mountain's valley above the summit's edge.
    #[test]
    fn the_far_model_is_band_limited() {
        let texel = std::f32::consts::TAU / 1024.0;
        let whole = (0.0, 360.0);
        for (kind, top, bottom, az) in [
            ("grass", 0.3, 8.0, whole),
            ("sand", 0.3, 8.0, whole),
            ("snow", 0.3, 8.0, whole),
            ("rock", 0.3, 8.0, whole),
            ("road", 0.3, 8.0, (50.0, 130.0)),
            ("plaza", 0.3, 8.0, whole),
            ("mountain", 0.3, 2.0, whole),
            ("lake", 0.3, 3.5, (160.0, 200.0)),
        ] {
            let model = FloorModel::new(&floor_of(kind), false);
            let limited = texel_noise(&model, 1024, top, bottom, az, texel);
            let exact = texel_noise(&model, 1024, top, bottom, az, 0.0);
            println!("{kind}: texel-to-texel {limited:.5} band-limited, {exact:.5} exact");
            assert!(limited < 0.01, "{kind}: {limited}");
            assert!(exact > 2.0 * limited, "{kind}: the exact model is noisier, so the bound means something ({exact})");
        }
    }

    /// Plan hdri-8 §3.5: a road runs through the viewer's place along its heading, both ways. At each depression its
    /// band is asin(half width x tan(depression) / eye) to each side of the heading and of the heading + 180 (its
    /// edges where the lateral distance is half its width), so it narrows to its two vanishing points on the horizon.
    #[test]
    fn the_road_band_narrows_to_its_vanishing_points() {
        let floor = FloorParams { heading_deg: 30.0, markings: false, ..floor_of("road") };
        let model = FloorModel::new(&floor, false).without_variation();
        let asphalt = model.sample(dir_from_az_el(30.0, -45.0), 0.0).albedo;
        let half = 0.5 * floor.lanes as f32 * floor::LANE_WIDTH_M;
        let mut last = f32::INFINITY;
        for depression in [20.0f32, 10.0, 3.0, 1.0, 0.3] {
            let want = (half * depression.to_radians().tan() / floor.eye_m).asin().to_degrees();
            for centre in [30.0f32, 210.0] {
                for side in [-1.0f32, 1.0] {
                    // walk out from the centre line in 0.001 degree steps to the first texel that is not asphalt
                    let mut off = 0.0f32;
                    while model.sample(dir_from_az_el(centre + side * off, -depression), 0.0).albedo == asphalt {
                        off += 0.001;
                        assert!(off < 90.0, "no edge");
                    }
                    assert!((off - want).abs() < 0.01, "{depression} degrees down, about {centre}: the edge at {off}, not {want}");
                }
            }
            assert!(want < last, "it narrows toward the horizon");
            last = want;
        }
        // across the turn from the road it is the field
        let field = model.sample(dir_from_az_el(120.0, -5.0), 0.0).albedo;
        assert!(field != asphalt);
    }

    /// Road bends (2026-11-07): the road's frame is the straight one up to the bend's start, then follows a quarter
    /// turn of the radius (to the right: east of a north heading), then runs straight on; a point on the centre line
    /// reads as across 0, along its distance, whatever the piece. With the start behind the viewer, the viewer stands
    /// on the bend and the road's direction at the viewer is still the heading.
    #[test]
    fn a_bent_road_turns_a_quarter_round_its_radius_and_runs_on() {
        let straight = super::RoadPath::new(0.0, 0.0, 30.0);
        let f = straight.frame(2.0, -10.0);
        assert_eq!((f.v, f.u, f.t), (-2.0, 10.0, [0.0, -1.0]), "a north road: + across is west, + along is north");
        let right = super::RoadPath::new(0.0, 100.0, 30.0);
        assert!(right.bends());
        // before the bend the frames agree
        let (a, b) = (straight.frame(1.5, -20.0), right.frame(1.5, -20.0));
        assert!((a.v - b.v).abs() < 1.0e-4 && (a.u - b.u).abs() < 1.0e-4 && a.t == b.t, "{a:?} {b:?}");
        // the bend's end: a quarter turn of 100 m from 30 m ahead ends 130 m up the road, 100 m east of the straight
        // line (the centre at (100, -30)), heading east
        let (end, t) = right.point(30.0 + 100.0 * std::f32::consts::FRAC_PI_2);
        assert!((end[0] - 100.0).abs() < 1.0e-3 && (end[1] + 130.0).abs() < 1.0e-3, "{end:?}");
        assert!((t[0] - 1.0).abs() < 1.0e-4 && t[1].abs() < 1.0e-4, "{t:?}");
        // past it the road runs east: 50 m on is (150, -130)
        let (on, _) = right.point(30.0 + 100.0 * std::f32::consts::FRAC_PI_2 + 50.0);
        assert!((on[0] - 150.0).abs() < 1.0e-3 && (on[1] + 130.0).abs() < 1.0e-3, "{on:?}");
        // the centre line reads as across 0, along u, on every piece; 2 m to its left as across +2
        for u in [-40.0f32, 0.0, 29.0, 31.0, 100.0, 180.0, 220.0, 400.0] {
            let (p, t) = right.point(u);
            let f = right.frame(p[0], p[1]);
            assert!(f.v.abs() < 1.0e-2 && (f.u - u).abs() < 1.0e-2, "{u}: {f:?}");
            assert!((f.t[0] - t[0]).abs() < 1.0e-3 && (f.t[1] - t[1]).abs() < 1.0e-3, "{u}: {f:?} against {t:?}");
            let n = super::RoadPath::left_of(t);
            let g = right.frame(p[0] + 2.0 * n[0], p[1] + 2.0 * n[1]);
            assert!((g.v - 2.0).abs() < 1.0e-2 && (g.u - u).abs() < 1.0e-2, "{u}: {g:?}");
        }
        // to the left the bend is mirrored: its end 100 m west
        let left = super::RoadPath::new(0.0, -100.0, 30.0);
        let (end, _) = left.point(30.0 + 100.0 * std::f32::consts::FRAC_PI_2);
        assert!((end[0] + 100.0).abs() < 1.0e-3 && (end[1] + 130.0).abs() < 1.0e-3, "{end:?}");
        // the viewer on the bend: the road's direction at the viewer is the heading, the arc began 50 m behind, and
        // behind that it is straight along the way it had there
        let on_it = super::RoadPath::new(0.0, 100.0, -50.0);
        let (p0, t0) = on_it.point(0.0);
        assert!(p0[0].abs() < 1.0e-4 && p0[1].abs() < 1.0e-4 && (t0[1] + 1.0).abs() < 1.0e-4, "{p0:?} {t0:?}");
        let (p1, t1) = on_it.point(-50.0);
        // (the arc's centre lies 100 m east: behind the viewer too the road bends toward it)
        assert!((p1[0] - (100.0 - 100.0 * 0.5f32.cos())).abs() < 1.0e-2 && (p1[1] - 100.0 * 0.5f32.sin()).abs() < 1.0e-2, "{p1:?}");
        let (p2, t2) = on_it.point(-80.0);
        assert!((t2[0] - t1[0]).abs() < 1.0e-4 && (t2[1] - t1[1]).abs() < 1.0e-4, "straight before the arc");
        assert!(((p2[0] - p1[0]) + 30.0 * t1[0]).abs() < 1.0e-2 && ((p2[1] - p1[1]) + 30.0 * t1[1]).abs() < 1.0e-2, "{p2:?}");
        // the far model follows it: 150 m up a road bending right from 30 m, the asphalt lies east of north
        let floor = FloorParams { curve: Some("right".to_string()), curve_radius_m: Some(100.0), curve_start_m: Some(30.0), markings: false, ..floor_of("road") };
        let model = FloorModel::new(&floor, false).without_variation();
        let asphalt = model.sample(dir_from_az_el(0.0, -45.0), 0.0).albedo;
        let (p, _) = model.road().point(150.0);
        let (az, el) = az_el_from_dir(vec3f(p[0], -floor.eye_m, p[1]).normalize());
        assert!(az > 20.0 && az < 90.0, "150 m along, the road lies east of north: {az}");
        assert_eq!(model.sample(dir_from_az_el(az, el), 0.0).albedo, asphalt, "asphalt there");
        assert_ne!(model.sample(dir_from_az_el(0.0, el), 0.0).albedo, asphalt, "and the field straight north at that depression");
    }

    #[test]
    fn night_visibility_depends_on_distance_and_haze() {
        let floor = floor_of("road");
        let clear = FloorModel::new(&floor, false);
        let hazy = FloorModel::new(&floor, false).with_haze(3.0);
        // by night the ground's light runs on: nearly all of it from 100 m, over half from 10 km in clear air, less in
        // haze; none at or above the horizon
        let look = |r: f32| vec3f(0.0, -floor.eye_m, -r).normalize();
        assert!(clear.night_keep(look(100.0)) > 0.99 && clear.night_keep(look(10_000.0)) > 0.55 && clear.night_keep(look(10_000.0)) < 0.65, "{} {}", clear.night_keep(look(100.0)), clear.night_keep(look(10_000.0)));
        assert!(hazy.night_keep(look(10_000.0)) < 0.25, "{}", hazy.night_keep(look(10_000.0)));
        assert_eq!(clear.night_keep(vec3f(0.0, 0.0, -1.0)), 0.0);
    }

    /// Plan hdri-8 §3.8 (D2: one water model): the far lake's open water is the weather's full-water ground for the
    /// same direction, its bed the floor's: a flat Lake (Ripples 0) under a clear noon sky equals, within 1e-5, the
    /// floorless map under a clear weather with Wet ground 1.00 and the lake bed's colour for its ground.
    #[test]
    fn the_lake_water_mirrors_the_sky() {
        use super::super::presets::preset;
        for name in ["Clear noon", "Sunset", "Moonlit night"] {
            let base = preset(name).unwrap();
            let lake = FloorParams { lake_ripples: 0.0, shore: 0.0, far_shore_m: 5000.0, ..floor_of("lake") };
            let floored = Env::new(&with_floor(&base, Some(lake)));
            let mut wet = base.clone();
            let tint = FloorParams::default().tint;
            let bed = FloorModel::recipe(FloorKind::Lake).alt;
            wet.sky.atmosphere.ground_color = [bed[0] * tint[0], bed[1] * tint[1], bed[2] * tint[2]];
            wet.sky.weather = Some(WeatherParams { kind: "clear".to_string(), wet_ground: 1.0, ..WeatherParams::default() });
            let puddled = Env::new(&wet);
            let mut worst = 0.0f32;
            // by night the floor's water runs on through the night air (`FloorModel::night_keep`) where the old
            // ground fades out over the 2 degrees under the horizon: the same below that band, to the air's share
            let night = name == "Moonlit night";
            for depression in [0.5f32, 2.0, 5.0, 15.0, 40.0, 80.0] {
                if night && depression < 2.0 {
                    continue;
                }
                for az in [-60.0f32, -20.0, 0.0, 35.0, 70.0] {
                    let d = dir_from_az_el(floor::LAKE_FRONT_AZ_DEG + az, -depression);
                    let (a, b) = (floored.radiance(d), puddled.radiance(d));
                    worst = worst.max((a - b).abs().max_elem() / a.max_elem().max(1.0e-6));
                }
            }
            println!("{name}: the lake is the full-water ground within {worst:e}");
            assert!(worst <= if night { 5.0e-3 } else { 1.0e-5 }, "{name}: {worst}");
        }
    }

    /// Plan hdri-8 §3.8: a Lake mirrors the sky's discs, so their reflections are refined as the puddles' are, and a
    /// frozen one (snow weather) is ice: glossier than snow and still a mirror, the sun's glint in it.
    #[test]
    fn a_lake_freezes_under_snow_weather() {
        use super::super::presets::preset;
        let mut snowing = preset("Clear noon").unwrap();
        snowing.sky.weather = Some(WeatherParams { kind: "snow".to_string(), rain: 0.0, ..WeatherParams::default() });
        let env = Env::new(&with_floor(&snowing, Some(floor_of("lake"))));
        assert!(env.floor.as_ref().unwrap().frozen());
        let d = dir_from_az_el(floor::LAKE_FRONT_AZ_DEG, -10.0);
        let far = env.floor.as_ref().unwrap().sample(d, 0.0);
        assert_eq!((far.water, far.ice), (0.0, 1.0));
        let open = Env::new(&with_floor(&preset("Clear noon").unwrap(), Some(FloorParams { lake_ripples: 0.0, ..floor_of("lake") })));
        assert!(!open.floor.as_ref().unwrap().frozen());
        let bare = Env::new(&preset("Clear noon").unwrap()).hot_spots().len();
        assert!(open.hot_spots().len() == 2 * bare, "a flat lake's sun is refined as the sun is");
        let rippled = Env::new(&with_floor(&preset("Clear noon").unwrap(), Some(floor_of("lake"))));
        assert!(rippled.hot_spots().len() == bare, "a rippled lake's sun is its smooth glitter");
        let mut no_floor = snowing.clone();
        no_floor.floor = None;
        assert_eq!(env.hot_spots().len(), 2 * Env::new(&no_floor).hot_spots().len(), "the ice mirrors the sun");
        assert!(env.bake(128).data.iter().flatten().all(|v| v.is_finite()));
    }

    /// clamp() holds every floor field to its range: NaN to its default, infinities and strays to the nearest bound,
    /// the heading wrapped, an unknown kind or pattern to the first, each change reported under `floor.`.
    #[test]
    fn the_floor_block_is_clamped_field_by_field() {
        let d = FloorParams::default();
        let floats: [(&str, fn(&mut FloorParams) -> &mut f32, (f32, f32)); 20] = [
            ("detail_m", |f| &mut f.detail_m, FLOOR_DETAIL_RANGE),
            ("eye_m", |f| &mut f.eye_m, FLOOR_EYE_RANGE),
            ("grass_length_m", |f| &mut f.grass_length_m, FLOOR_GRASS_LENGTH_RANGE),
            ("dryness", |f| &mut f.dryness, FLOOR_DRYNESS_RANGE),
            ("wind", |f| &mut f.wind, FLOOR_WIND_RANGE),
            ("ripples", |f| &mut f.ripples, FLOOR_RIPPLES_RANGE),
            ("dunes", |f| &mut f.dunes, FLOOR_DUNES_RANGE),
            ("softness", |f| &mut f.softness, FLOOR_SOFTNESS_RANGE),
            ("sparkle", |f| &mut f.sparkle, FLOOR_SPARKLE_RANGE),
            ("stone_m", |f| &mut f.stone_m, FLOOR_STONE_RANGE),
            ("gravel", |f| &mut f.gravel, FLOOR_GRAVEL_RANGE),
            ("wear", |f| &mut f.wear, FLOOR_WEAR_RANGE),
            ("tile_m", |f| &mut f.tile_m, FLOOR_TILE_RANGE),
            ("drop_m", |f| &mut f.drop_m, FLOOR_DROP_RANGE),
            ("ruggedness", |f| &mut f.ruggedness, FLOOR_RUGGEDNESS_RANGE),
            ("snow_line", |f| &mut f.snow_line, FLOOR_SNOW_LINE_RANGE),
            ("far_shore_m", |f| &mut f.far_shore_m, FLOOR_FAR_SHORE_RANGE),
            ("lake_ripples", |f| &mut f.lake_ripples, FLOOR_LAKE_RIPPLES_RANGE),
            ("shore", |f| &mut f.shore, FLOOR_SHORE_RANGE),
            ("heading_deg", |f| &mut f.heading_deg, (0.0, 360.0)),
        ];
        let clamped = |f: FloorParams| {
            let mut p = HdriParams { floor: Some(f), ..HdriParams::default() };
            let report = p.clamp();
            (p.floor.unwrap(), report)
        };
        for (field, get, (lo, hi)) in floats {
            let path = format!("floor.{field}");
            let mut f = d.clone();
            *get(&mut f) = f32::NAN;
            let (mut held, report) = clamped(f);
            assert_eq!(*get(&mut held), *get(&mut d.clone()), "{field}: NaN is the default");
            assert_eq!(report, vec![path.clone()]);
            if field == "heading_deg" {
                let mut f = d.clone();
                f.heading_deg = 370.0;
                assert_eq!(clamped(f).0.heading_deg, 10.0);
                let mut f = d.clone();
                f.heading_deg = f32::INFINITY;
                assert_eq!(clamped(f).0.heading_deg, 0.0);
                continue;
            }
            for (given, want) in [(f32::INFINITY, hi), (f32::NEG_INFINITY, lo), (hi + 1.0, hi), (lo - 1.0, lo)] {
                let mut f = d.clone();
                *get(&mut f) = given;
                let (mut held, report) = clamped(f);
                assert_eq!(*get(&mut held), want, "{field}: {given}");
                assert_eq!(report, vec![path.clone()], "{field}: {given}");
            }
            let mut f = d.clone();
            *get(&mut f) = 0.5 * (lo + hi);
            assert!(clamped(f).1.is_empty(), "{field}: in range");
        }
        let f = FloorParams { tint: [-1.0, 2.0, f32::NAN], kind: " Lake ".to_string(), pattern: "zigzag".to_string(), seed: 1_000_000, lanes: 9, ..d.clone() };
        let (held, report) = clamped(f);
        assert_eq!(held.tint, [0.0, 1.0, 1.0]);
        assert_eq!((held.kind.as_str(), held.kind()), ("lake", FloorKind::Lake));
        assert_eq!((held.pattern.as_str(), held.pattern()), ("square", FloorPattern::Square));
        assert_eq!((held.seed, held.lanes), (FLOOR_SEED_MAX, FLOOR_LANES_RANGE.1));
        assert_eq!(report, ["floor.kind", "floor.seed", "floor.tint", "floor.lanes", "floor.pattern"]);
        let (held, _) = clamped(FloorParams { kind: "carpet".to_string(), lanes: 0, ..d.clone() });
        assert_eq!((held.kind(), held.lanes), (FloorKind::Grass, 1));
        assert!(clamped(d.clone()).1.is_empty(), "the defaults are in range");
        for kind in FloorKind::ALL {
            assert_eq!(floor_of(kind.as_str()).kind(), kind);
        }
    }

    /// A map whose floor is NaN everywhere bakes finite for every kind.
    #[test]
    fn a_nan_floor_bakes_finite() {
        use super::super::presets::preset;
        let nan = f32::NAN;
        for kind in FLOOR_KIND_NAMES {
            let f = FloorParams {
                tint: [nan; 3], detail_m: nan, eye_m: nan, grass_length_m: nan, dryness: nan, wind: nan, ripples: nan, dunes: nan, softness: nan,
                sparkle: nan, stone_m: nan, gravel: nan, heading_deg: nan, wear: nan, tile_m: nan, drop_m: nan, ruggedness: nan, snow_line: nan,
                far_shore_m: nan, lake_ripples: nan, shore: nan, ..floor_of(kind)
            };
            let model = FloorModel::new(&f, false);
            assert_eq!(model.eye_m(), FloorParams::default().eye_m);
            let map = Env::new(&with_floor(&preset("Clear noon").unwrap(), Some(f))).bake(64);
            assert!(map.data.iter().flatten().all(|v| v.is_finite()), "{kind}");
        }
    }

    /// The floor block's JSON: it round-trips, a file from before it loads with no floor, a patch creates a whole
    /// block from its template and names its paths, and a tint of the wrong length is refused by name.
    #[test]
    fn the_floor_block_round_trips_and_old_files_load_without_one() {
        let mut p = super::super::presets::preset("Clear noon").unwrap();
        p.floor = Some(FloorParams { kind: "road".to_string(), heading_deg: 45.0, lanes: 3, markings: false, tint: [0.9, 0.8, 0.7], ..FloorParams::default() });
        assert_eq!(HdriParams::from_json(&p.to_json()).unwrap(), p);
        let old = HdriParams::from_json(&super::super::presets::preset("Clear noon").unwrap().to_json()).unwrap();
        assert_eq!(old.floor, None, "a file from before the floor has none");
        // (but a hybrid studio standing on the ground: a tunnel's or a bridge's floor is the ground's)
        for name in super::super::presets::PRESET_NAMES {
            let p = super::super::presets::preset(name).unwrap();
            let on_ground = p.studio.room.as_ref().is_some_and(|room| room.open_floor());
            assert_eq!(p.floor.is_some(), on_ground, "{name}: a floor exactly where a room stands on the ground");
        }
        let (merged, report) = old.merge_json(r#"{"floor": {"kind": "sand", "dunes": 0.7, "seed": 12.6}}"#).unwrap();
        let floor = merged.floor.unwrap();
        assert_eq!((floor.kind(), floor.dunes, floor.seed, floor.on), (FloorKind::Sand, 0.7, 13, true));
        assert_eq!(floor.ripples, FloorParams::default().ripples, "the rest from the template");
        assert!(report.iter().any(|r| r == "floor.seed"), "{report:?}");
        let (_, report) = old.merge_json(r#"{"floor": {"bogus": 1}}"#).unwrap();
        assert!(report.iter().any(|r| r.contains("floor.bogus")), "{report:?}");
        let err = old.merge_json(r#"{"floor": {"tint": [1, 1]}}"#).unwrap_err();
        assert!(err.contains("floor.tint"), "{err}");
        let (off, _) = p.merge_json(r#"{"floor": null}"#).unwrap();
        assert_eq!(off.floor, None);
    }

    // ---- plan hdri-8 T2: planes, the floor's light and the composition ----------------------------------------------

    use super::{FloorHere, FloorPlanes, FloorTexel};
    use std::sync::Arc;

    /// A sky lit by a manual sun at `elevation` degrees, azimuth 135, with a floor of `kind`.
    fn sun_floor(elevation: f32, kind: &str) -> HdriParams {
        let mut p = super::super::presets::preset("Clear noon").unwrap();
        p.sky.sun.mode = "manual".to_string();
        p.sky.sun.elevation_deg = elevation;
        p.sky.sun.azimuth_deg = 135.0;
        p.floor = Some(floor_of(kind));
        p
    }

    /// A level texel of `albedo` lit by the key as a level ground is (`key` its term: the key's sine), dry.
    fn level(albedo: [f32; 3], key: f32) -> FloorTexel {
        FloorTexel { albedo, key, normal: [0.0, 1.0, 0.0], dist: 10.0, ..FloorTexel::default() }
    }

    /// Planes `w` wide for `env`'s floor, reaching past twice its reach, of `texel` at each texel's centre.
    fn planes_for(env: &Env, w: usize, texel: impl Fn(Vec3f) -> FloorTexel) -> Arc<FloorPlanes> {
        let floor = env.floor.as_ref().unwrap();
        let far_deg = (floor.eye_m() / (2.0 * floor.reach_m())).atan().to_degrees();
        let (h, top) = FloorPlanes::rows_for(w, far_deg);
        Arc::new(FloorPlanes::from_fn(w, h, top, floor.eye_m(), texel))
    }

    /// The sine of the floor's key above the horizon (0 without one).
    fn key_sine(env: &Env) -> f32 {
        env.floor_light().and_then(|light| light.key_dir).map_or(0.0, |k| k[1].max(0.0))
    }

    /// Directions inside the planes' full share: depressions from 8 to 80 degrees, all the way round.
    fn near_dirs() -> Vec<Vec3f> {
        let mut dirs = Vec::new();
        for depression in [8.0f32, 15.0, 30.0, 55.0, 80.0] {
            for k in 0..12 {
                dirs.push(dir_from_az_el(k as f32 * 30.0 + 7.0, -depression));
            }
        }
        dirs
    }

    fn worst_relative(a: &[Vec3f], b: &[Vec3f]) -> f32 {
        a.iter().zip(b).map(|(a, b)| (*a - *b).abs().max_elem() / a.max_elem().max(1.0e-6)).fold(0.0, f32::max)
    }

    /// Plan hdri-8 §6: a flat floor is the old ground. A plane texel with an upward normal, no occlusion, no shadow, no
    /// hollow and the old ground colour for albedo composes to the floorless map's ground: by day, under an overcast,
    /// by night (the moon the key) and under rain on a wet ground (whose puddles a texel with no hollow does not hold:
    /// the old damp ground between them: the floor is Sand, whose film is the plain ground's grains, `damp_films`).
    /// Exactly, within 1e-5, through the composition itself (`sky_layers_on`, the ground relit by the cover, and the
    /// weather's wet ground); and through packed planes attached to the map within their 8-bit key term (1 %).
    #[test]
    fn a_flat_floor_is_the_old_ground() {
        use super::super::presets::preset;
        let mut wet = preset("Clear noon").unwrap();
        wet.sky.weather = Some(WeatherParams { kind: "rain".to_string(), wet_ground: 0.7, ..WeatherParams::default() });
        for (name, mut p) in [("sun", preset("Clear noon").unwrap()), ("overcast", preset("Overcast").unwrap()), ("night", preset("Moonlit night").unwrap()), ("wet", wet)] {
            // a ground colour 8-bit sRGB holds exactly, so the packed planes carry it as it is
            let code = FloorTexel::unpack(&level(p.sky.atmosphere.ground_color, 0.0).pack()).albedo;
            p.sky.atmosphere.ground_color = code;
            let old = Env::new(&p);
            // The sand's far model tinted so its mean albedo (what the sky's ground bounce reads, 30 degrees down) is
            // the old ground's: the atmosphere over the floor is then the floorless map's.
            let mut sand = floor_of("sand");
            let mean = FloorModel::new(&sand, false).mean_albedo(30f32.to_radians());
            sand.tint = std::array::from_fn(|c| sand.tint[c] * code[c] / mean[c]);
            let mut floored = with_floor(&p, Some(sand));
            floored.floor.as_mut().unwrap().eye_m = 2.0;
            let env = Env::new(&floored);
            let key = key_sine(&env);
            assert!(key > 0.0, "{name}: the key is up");
            let atmo = env.atmo.as_ref().unwrap();
            // the ground behind the weather's veil (the veil is the weather's, the same over both), without puddles
            let behind = Sampling { damp_ground: true, unveiled: true, ..Sampling::default() };
            let (mut want, mut got) = (Vec::new(), Vec::new());
            for d in near_dirs() {
                let mut far = env.floor.as_ref().unwrap().sample(d, 0.0);
                far.shade = 1.0;
                let here = FloorHere { far, near: Some((level(code, key), 1.0)) };
                let c = weather::relit_ground(d, env.sky_layers_on(d, atmo, None, Sampling::default(), Some(&here)), env.ground_gain);
                let c = match &env.weather {
                    // the mirror shows the sky as the weather shows it (`base_layer`'s `seen`; no strip here), the damp
                    // ground's sheen through its film's facets (`Env::damp_sheen`)
                    Some(weather) => {
                        let seen = |v: Vec3f| weather.veil(v, env.sky_layers(v, atmo, None, Sampling::default()), env.weather_sun, env.dome) + weather.lightning(v, 0.0);
                        let sheen = || env.damp_sheen(d, behind);
                        weather.ground_sheened(d, env.sky_layers_on(d, atmo, None, Sampling::default(), Some(&here)), env.ground_gain, &seen, 0.0, None, &sheen)
                    }
                    None => c,
                };
                got.push(c);
                want.push(old.base_layer(d, None, behind));
            }
            let exact = worst_relative(&want, &got);
            let mut env = env.with_floor(planes_for(&Env::new(&floored), 512, |_| level(code, key)));
            env.floor = env.floor.take().map(|floor| floor.without_variation());
            let attached: Vec<Vec3f> = near_dirs().iter().map(|&d| env.base_layer(d, None, behind)).collect();
            let packed = worst_relative(&want, &attached);
            println!("{name}: the flat floor is the old ground within {exact:e} composed, {packed:e} through packed planes");
            // by night the floor's ground runs on through the night air (`FloorModel::night_keep`): the old ground to
            // the air's share (the near directions all lie under the glow's 2 degree band)
            assert!(exact <= if name == "night" { 5.0e-3 } else { 1.0e-5 }, "{name}: {exact}");
            assert!(packed <= 0.01, "{name}: {packed}");
        }
    }

    /// Plan hdri-8 T2: the key's term shapes the floor: under a low sun a face turned toward it is brighter than a
    /// level one, and a shadowed one darker; the sky's light on the tilted face is the L1 sky's.
    #[test]
    fn a_slope_toward_the_key_is_brighter_and_a_shadow_darker() {
        let p = sun_floor(12.0, "rock");
        let env = Env::new(&p);
        let sun = vec(env.floor_light().unwrap().key_dir.unwrap());
        let level_key = sun.y;
        // a face tilted 25 degrees toward the sun's azimuth
        let toward = vec3f(sun.x, 0.0, sun.z).normalize();
        let n = (vec3f(0.0, 1.0, 0.0) * 25f32.to_radians().cos() + toward * 25f32.to_radians().sin()).normalize();
        let sector = |d: Vec3f| ((az_el_from_dir(d).0 / 120.0) as usize) % 3;
        let a = [0.3, 0.3, 0.3];
        let planes = planes_for(&env, 512, |d| match sector(d) {
            0 => level(a, level_key),
            1 => FloorTexel { normal: [n.x, n.y, n.z], key: n.dot(sun), ..level(a, 0.0) },
            _ => level(a, 0.0),
        });
        let env = env.with_floor(planes);
        let lum = |az: f32| crate::sky::luminance(env.radiance(dir_from_az_el(az, -40.0)));
        let (flat, slope, shadow) = (lum(60.0), lum(180.0), lum(300.0));
        println!("level {flat:.4}, toward the sun {slope:.4}, shadowed {shadow:.4}");
        assert!(slope > 1.5 * flat, "{slope} against {flat}");
        assert!(shadow < 0.8 * flat, "{shadow} against {flat}");
        // the sky's light on the tilted face: near the level face's, by the sky's shape. A low sun's sky is brighter
        // toward it (its aureole), and real ones are as much: the same L1 tilt, 25 degrees toward a 12-17 degree sun,
        // measured on real HDRIs comes to 1.32 (sunny_vondelpark, red), 1.20 (rural_crossroads), 1.13 (chapmans_drive);
        // the calibrated sky's is 1.22 red, 1.16 green, 1.08 blue (before the calibration, under 1.2).
        let tilt = env.floor_lit.tilt([n.x, n.y, n.z]);
        assert!(tilt.max_elem() < 1.35 && tilt.min_elem() > 0.8 && tilt != Vec3f::all(1.0), "{tilt:?}");
        assert_eq!(env.floor_lit.tilt([0.0, 1.0, 0.0]), Vec3f::all(1.0));
    }

    /// A strip for the water's mirror: dark city blocks 0 to `tall` degrees up (2 to 8 by column, 8 x their distance),
    /// nothing under the horizon.
    fn mirror_band() -> Arc<skyline::SkylineBand> {
        let (w, h, top, bottom) = (96usize, 22usize, 10.0f32, -1.0f32);
        let row = (top - bottom) / h as f32;
        let mut texels = Vec::new();
        let mut dist = Vec::new();
        for y in 0..h {
            let el = top - (y as f32 + 0.5) * row;
            for x in 0..w {
                let tall = 2.0 + 6.0 * ((x * 7 % 11) as f32 / 10.0);
                let city = el > 0.0 && el < tall;
                texels.push(if city { [0.01, 0.01, 0.012, 1.0] } else { [0.0; 4] });
                dist.push(if city { 1500.0 } else { 0.0 });
            }
        }
        Arc::new(skyline::SkylineBand::new(w, h, texels, dist, top, bottom).with_eye(6.3))
    }

    /// Plan hdri-8 T2: water in the planes mirrors the sky as the weather shows it, and the strip where it stands: the
    /// city in the lake. Under a thin fog, a water texel whose mirror finds a city block is darker than the same
    /// texel without the strip, and one whose mirror finds open sky is as it was.
    #[test]
    fn the_planes_water_mirrors_the_sky_and_the_strip() {
        let mut p = sun_floor(35.0, "rock");
        p.sky.weather = Some(WeatherParams { kind: "clear".to_string(), fog: 0.02, ..WeatherParams::default() });
        p.skyline = Some(SkylineParams { on: true, seed: 1, turn_deg: 0.0, lights_by_day: 0.0, settings: Default::default() });
        let water = |env: &Env| planes_for(env, 512, |_| FloorTexel { water: 1.0, ..level([0.2, 0.2, 0.2], 0.5) });
        let bare = Env::new(&p);
        let bare = bare.with_floor(water(&Env::new(&p)));
        let city = Env::new(&p).with_skyline(mirror_band()).with_floor(water(&Env::new(&p)));
        // the band's column x is centred at azimuth ((x + 0.5) / 96 - 0.5) x 360 (its turn is 0)
        let (mut darker, mut same) = (0, 0);
        for x in 0..96 {
            let tall = 2.0 + 6.0 * ((x * 7 % 11) as f32 / 10.0);
            let az = ((x as f32 + 0.5) / 96.0 - 0.5) * 360.0;
            let d = dir_from_az_el(az, -6.0);
            let (a, b) = (crate::sky::luminance(bare.radiance(d)), crate::sky::luminance(city.radiance(d)));
            if tall > 7.0 {
                assert!(b < 0.9 * a, "column {x}: the city in the lake {b} against the sky's {a}");
                darker += 1;
            } else if tall < 5.0 {
                assert!((b - a).abs() <= 1.0e-4 * a, "column {x}: open sky {b} against {a}");
                same += 1;
            }
        }
        println!("{darker} columns mirror the city, {same} the open sky");
        assert!(darker > 5 && same > 5);
    }

    /// Plan hdri-8 T2: the weather's veil lies over the floor once: the floor as drawn is the veil over the floor
    /// behind it (`unveiled`), exactly, with planes attached, water in them and puddles in their hollows.
    #[test]
    fn the_veil_lies_over_the_floor_once() {
        let mut p = sun_floor(20.0, "rock");
        p.sky.weather = Some(WeatherParams { kind: "rain".to_string(), fog: 0.3, wet_ground: 0.6, ..WeatherParams::default() });
        let env = Env::new(&p);
        let env = env.with_floor(planes_for(&Env::new(&p), 512, |d| FloorTexel { hollow: if d.x > 0.0 { 0.9 } else { 0.0 }, water: if d.z > 0.5 { 1.0 } else { 0.0 }, ..level([0.25, 0.22, 0.2], 0.3) }));
        let weather = env.weather.as_ref().unwrap();
        for d in near_dirs() {
            let drawn = env.base_layer(d, None, Sampling::default());
            let behind = env.base_layer(d, None, Sampling { unveiled: true, ..Sampling::default() });
            let once = weather.veil(d, behind, env.weather_sun, env.dome) + weather.lightning(d, 0.0);
            assert!((drawn - once).abs().max_elem() <= 1.0e-6 * drawn.max_elem().max(1.0e-6), "{d:?}: {drawn:?} against {once:?}");
            assert!(drawn != behind, "the veil is there");
        }
    }

    /// Plan hdri-8 §4.3: the key is the same with and without planes, by day and by night.
    #[test]
    fn the_key_is_the_same_with_and_without_planes() {
        use super::super::presets::preset;
        let mut night = preset("Moonlit night").unwrap();
        night.floor = Some(floor_of("snow"));
        for p in [sun_floor(30.0, "grass"), sun_floor(2.0, "lake"), night] {
            let env = Env::new(&p);
            let key = env.sun();
            let light = env.floor_light();
            let env = env.with_floor(planes_for(&Env::new(&p), 256, |_| level([0.9, 0.9, 0.9], 1.0)));
            assert_eq!(env.sun(), key);
            assert_eq!(env.floor_light(), light);
            assert!(key.is_some());
        }
    }

    /// Plan hdri-8 §4.3: the bare lookups never read the planes: the strip's light (its rings, its ground ring, its
    /// damp ring), the dome, the ground gain, the floor's own light and every bare look are the same with planes.
    #[test]
    fn bare_lookups_never_read_the_planes() {
        let mut p = sun_floor(25.0, "lake");
        p.sky.weather = Some(WeatherParams { kind: "rain".to_string(), wet_ground: 0.6, ..WeatherParams::default() });
        p.skyline = Some(SkylineParams { on: true, seed: 1, turn_deg: 30.0, lights_by_day: 0.0, settings: Default::default() });
        let without = Env::new(&p).with_skyline(mirror_band());
        let with = Env::new(&p).with_skyline(mirror_band()).with_floor(planes_for(&Env::new(&p), 512, |_| FloorTexel { hollow: 1.0, water: 0.5, ..level([0.9, 0.1, 0.1], 0.0) }));
        assert!(without.skyline_light() == with.skyline_light(), "the strip's light");
        assert_eq!((without.dome, without.ground_gain, without.floor_lit), (with.dome, with.ground_gain, with.floor_lit));
        let bare = Sampling { bare: true, unveiled: true, even_ground: true, ..Sampling::default() };
        for d in near_dirs() {
            assert_eq!(without.radiance_with(d, None, bare), with.radiance_with(d, None, bare), "{d:?}");
            assert!(without.radiance(d) != with.radiance(d), "the map itself reads them");
        }
    }

    /// Plan hdri-8 §4.3: planes attached later are the map built with them, bit for bit, in either order with the
    /// strip, with a blooming caster in front of the floor (its bloom is rastered again).
    #[test]
    fn attached_planes_are_the_built_maps_bit_for_bit() {
        let mut p = sun_floor(20.0, "rock");
        p.skyline = Some(SkylineParams { on: true, seed: 1, turn_deg: 0.0, lights_by_day: 0.0, settings: Default::default() });
        p.lights.push(LightParams {
            azimuth_deg: 135.0,
            elevation_deg: 0.0,
            width_deg: 50.0,
            height_deg: 50.0,
            caster: Some(CasterParams { bloom_strength: 1.0, bloom_length_deg: 10.0, bloom_threshold_ev: -6.0, ..CasterParams::default() }),
            ..LightParams::new_caster()
        });
        let planes = planes_for(&Env::new(&p), 256, |d| level([0.2 + 0.5 * d.x.abs(), 0.3, 0.2], 0.4));
        let images = ImageSet { skyline: Some(mirror_band()), ..ImageSet::default() };
        let built = Env::new_with(&p, &images).with_floor(planes.clone());
        let floor_first = Env::new(&p).with_floor(planes.clone()).with_skyline(mirror_band());
        let strip_first = Env::new(&p).with_skyline(mirror_band()).with_floor(planes.clone());
        let map = built.bake(128);
        assert!(floor_first.bake(128) == map, "the floor, then the strip");
        assert!(strip_first.bake(128) == map, "the strip, then the floor");
        assert_eq!(floor_first.sun(), built.sun());
        assert!(built.bake(128) != Env::new_with(&p, &images).bake(128), "the planes are drawn");
    }

    /// Plan hdri-8 T2: planes whose values are NaN, infinite or arbitrary bytes bake a finite map, for every kind,
    /// by day, by night and under rain.
    #[test]
    fn nan_planes_stay_finite() {
        use super::super::presets::preset;
        let nan = f32::NAN;
        let mut rain = preset("Clear noon").unwrap();
        rain.sky.weather = Some(WeatherParams { kind: "rain".to_string(), wet_ground: 0.8, ..WeatherParams::default() });
        for base in [preset("Clear noon").unwrap(), preset("Moonlit night").unwrap(), rain] {
            for kind in FLOOR_KIND_NAMES {
                let p = with_floor(&base, Some(floor_of(kind)));
                let env = Env::new(&p);
                let poisoned = planes_for(&env, 128, |_| FloorTexel { albedo: [nan, f32::INFINITY, -1.0], key: nan, hollow: nan, water: nan, ice: f32::INFINITY, roughness: nan, sparkle: nan, normal: [nan, nan, nan], dist: nan });
                let mut state = 12345u32;
                let noise: Vec<[u8; 16]> = (0..poisoned.width() * poisoned.height())
                    .map(|_| std::array::from_fn(|_| {
                        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        (state >> 24) as u8
                    }))
                    .collect();
                let bytes = Arc::new(FloorPlanes::new(poisoned.width(), poisoned.height(), poisoned.top_deg(), 1.7, noise));
                for planes in [poisoned, bytes] {
                    let map = Env::new(&p).with_floor(planes).bake(64);
                    assert!(map.data.iter().flatten().all(|v| v.is_finite()), "{kind}");
                }
            }
        }
    }

    /// Plan hdri-8 §4.2: the planes are read bilinearly: at a texel's centre its own value, half way between two
    /// columns their mean; and a bake jittered half a texel at the planes' width reads that mean (the composition is
    /// linear in the albedo).
    #[test]
    fn a_jittered_bake_reads_the_planes_bilinearly() {
        let p = sun_floor(40.0, "rock");
        let env = Env::new(&p);
        let key = key_sine(&env);
        let w = 64usize;
        let column = |d: Vec3f| (((0.5 + d.x.atan2(-d.z) / std::f32::consts::TAU) * w as f32).floor() as usize) % w;
        let planes = planes_for(&env, w, |d| level(if column(d) % 2 == 0 { [0.1, 0.1, 0.1] } else { [0.5, 0.5, 0.5] }, key));
        let y = planes.height() - 8;
        let el = planes.top_deg() - (y as f32 + 0.5) * 360.0 / w as f32;
        for x in 0..4 {
            let az = ((x as f32 + 0.5) / w as f32 - 0.5) * 360.0;
            let (t, share) = planes.sample(dir_from_az_el(az, el)).unwrap();
            assert_eq!(share, 1.0);
            assert!((t.albedo[0] - planes.texel(x, y).albedo[0]).abs() < 1.0e-4, "column {x} at its centre");
            let (t, _) = planes.sample(dir_from_az_el(az + 180.0 / w as f32, el)).unwrap();
            let mean = 0.5 * (planes.texel(x, y).albedo[0] + planes.texel(x + 1, y).albedo[0]);
            assert!((t.albedo[0] - mean).abs() < 1.0e-3, "between {x} and {}", x + 1);
        }
        let mut env = env.with_floor(planes);
        env.floor = env.floor.take().map(|floor| floor.without_variation());
        let rows = |jitter: [f32; 2]| {
            let out = std::sync::Mutex::new(vec![[0.0f32; 4]; w * w / 2]);
            env.bake_rows_jittered_par(w, jitter, four_threads, &|row, texels| out.lock().unwrap()[row * w..(row + 1) * w].copy_from_slice(texels));
            out.into_inner().unwrap()
        };
        let (centred, half) = (rows([0.0, 0.0]), rows([0.5, 0.0]));
        let row = (w / 2) * 3 / 4;
        for x in 0..w - 1 {
            let want = 0.5 * (centred[row * w + x][1] + centred[row * w + x + 1][1]);
            let got = half[row * w + x][1];
            assert!((got - want).abs() <= 1.0e-3 * want, "texel {x}: {got} against {want}");
        }
    }

    /// Plan hdri-8 §3.8: a frozen lake's ice is glossy (it mirrors the sky by ice's Fresnel, so at grazing angles it
    /// outshines the same albedo without ice) and its snow is diffuse (looking down the two nearly agree).
    #[test]
    fn frozen_ice_is_glossy_and_snow_on_it_is_diffuse() {
        let mut p = sun_floor(20.0, "lake");
        p.sky.weather = Some(WeatherParams { kind: "snow".to_string(), rain: 0.0, ..WeatherParams::default() });
        let env = Env::new(&p);
        assert!(env.floor.as_ref().unwrap().frozen() && env.floor_light().unwrap().frozen);
        let a = [0.5, 0.55, 0.6];
        let planes = planes_for(&env, 512, |d| FloorTexel { ice: if d.x > 0.0 { 1.0 } else { 0.0 }, ..level(a, 0.2) });
        let env = env.with_floor(planes);
        let lum = |az: f32, depression: f32| crate::sky::luminance(env.radiance(dir_from_az_el(az, -depression)));
        // east (x > 0) is ice, west snow; the sun stands at azimuth 135 (south-east)
        for depression in [8.0f32, 12.0] {
            let (ice, snow) = (lum(90.0, depression), lum(270.0, depression));
            println!("{depression} degrees down: ice {ice:.4}, snow {snow:.4}");
            assert!(ice > snow, "{depression}: the ice mirrors");
        }
        let (ice, snow) = (lum(90.0, 80.0), lum(270.0, 80.0));
        assert!((ice - snow).abs() < 0.08 * snow, "looking down: {ice} against {snow}");
    }

    /// Plan hdri-8 §3.8: the sun's glitter on the lake's ripples is analytic, so its light holds across the bake's
    /// width: the light it adds to the map (the lake with its glitter less the lake without) is the same at 256, 512,
    /// 1024 and 2048 wide within 2 %, and so is a held bake's.
    #[test]
    fn the_sun_glitter_on_ripples_holds_its_light_across_widths() {
        let mut p = sun_floor(25.0, "lake");
        p.floor.as_mut().unwrap().lake_ripples = 0.6;
        let energy = |map: &EnvMap| {
            let (w, h) = (map.width, map.height);
            let mut sum = 0.0f64;
            for y in h / 2..h {
                let el = (0.5 - (y as f32 + 0.5) / h as f32) * std::f32::consts::PI;
                let cell = (std::f32::consts::TAU / w as f32) * (std::f32::consts::PI / h as f32) * el.cos();
                for x in 0..w {
                    sum += crate::sky::luminance(vec3f(map.data[y * w + x][0], map.data[y * w + x][1], map.data[y * w + x][2])) as f64 * cell as f64;
                }
            }
            sum
        };
        let glitter = |width: usize, held: bool| {
            let env = if held { Env::new(&p).with_held_floor() } else { Env::new(&p) };
            let mut dark = if held { Env::new(&p).with_held_floor() } else { Env::new(&p) };
            dark.glints.clear();
            energy(&env.bake_par(width, four_threads)) - energy(&dark.bake_par(width, four_threads))
        };
        let reference = glitter(2048, false);
        assert!(reference > 0.0);
        for (width, held) in [(256, false), (512, false), (1024, false), (512, true), (1024, true)] {
            let e = glitter(width, held);
            println!("{width} wide{}: the glitter's light {e:.4} ({:.3} of 2048's)", if held { ", held" } else { "" }, e / reference);
            assert!((e / reference - 1.0).abs() < 0.02, "{width}, held {held}: {e} against {reference}");
        }
    }

    /// Plan hdri-8 §3 and Q6, T2: a wet ground's puddles lie by the floor's kind, in its own hollows: none on Grass
    /// until the water rises between the blades, on Sand only in its troughs past 0.8, in the stones' hollows from the
    /// start; the planes' deep hollows hold water, their crowns none.
    #[test]
    fn puddles_lie_by_the_floors_kind_in_its_hollows() {
        use super::{hollow_cover, wet_cover};
        assert_eq!(wet_cover(FloorKind::Grass, 0.7, 1.0), 0.0);
        assert!(wet_cover(FloorKind::Grass, 0.95, 0.0) > 0.2);
        assert_eq!(wet_cover(FloorKind::Sand, 0.7, 1.0), 0.0);
        assert!(wet_cover(FloorKind::Sand, 0.88, 1.0) > 0.5);
        assert_eq!(wet_cover(FloorKind::Snow, 0.5, 1.0), 0.0);
        for kind in [FloorKind::Rock, FloorKind::Road, FloorKind::Plaza, FloorKind::Mountain, FloorKind::Lake] {
            assert_eq!(wet_cover(kind, 0.3, 0.7), 0.7, "{kind:?}");
        }
        assert!(hollow_cover(0.9, 0.3) > 0.9 && hollow_cover(0.1, 0.3) == 0.0 && hollow_cover(0.3, 1.0) == 1.0);
        // in the map: Rock under rain, the planes' east half hollows, the west crowns
        let mut p = sun_floor(30.0, "rock");
        p.sky.weather = Some(WeatherParams { kind: "clear".to_string(), wet_ground: 0.4, ..WeatherParams::default() });
        let env = Env::new(&p);
        let env = env.with_floor(planes_for(&Env::new(&p), 512, |d| FloorTexel { hollow: if d.x > 0.0 { 1.0 } else { 0.0 }, ..level([0.3, 0.3, 0.3], 0.5) }));
        let weather = env.weather.as_ref().unwrap();
        let (east, west) = (dir_from_az_el(90.0, -20.0), dir_from_az_el(270.0, -20.0));
        let cover = |d: Vec3f| env.floor_puddles(weather, d, &env.floor_at(d, Sampling::default()).unwrap(), weather::Puddles::Field);
        assert!(cover(east) > 0.999 && cover(west) == 0.0, "{} {}", cover(east), cover(west));
        let mut grass = p.clone();
        grass.floor = Some(floor_of("grass"));
        let env = Env::new(&grass);
        let env = env.with_floor(planes_for(&Env::new(&grass), 512, |_| FloorTexel { hollow: 1.0, ..level([0.1, 0.2, 0.05], 0.5) }));
        let weather = env.weather.as_ref().unwrap();
        assert_eq!(env.floor_puddles(weather, east, &env.floor_at(east, Sampling::default()).unwrap(), weather::Puddles::Field), 0.0, "grass holds no puddle at 0.4");
    }

    /// Plan hdri-8 §4.2: the packing round-trips within its codes, and the planes' handoff is made safe.
    #[test]
    fn plane_texels_pack_and_the_handoff_is_made_safe() {
        let t = FloorTexel { albedo: [0.2, 0.5, 0.9], key: 0.3, hollow: 0.6, water: 1.0, ice: 0.0, roughness: 0.7, sparkle: 0.1, normal: [0.3, 0.9, -0.2], dist: 123.4 };
        let u = FloorTexel::unpack(&t.pack());
        for (a, b) in t.albedo.iter().zip(u.albedo) {
            assert!((a - b).abs() < 0.01 * a, "{a} {b}");
        }
        assert!((u.key - 0.3).abs() < 0.003 && (u.dist - 123.4).abs() < 0.1 && (u.normal[0] - 0.3).abs() < 1.0e-4);
        assert!((u.normal[0] * u.normal[0] + u.normal[1] * u.normal[1] + u.normal[2] * u.normal[2] - 1.0).abs() < 1.0e-5);
        for h in [0u16, 1, 0x03ff, 0x0400, 0x3c00, 0x7bff] {
            assert_eq!(super::f32_to_f16(super::f16_to_f32(h)), h, "{h:#06x}");
        }
        let short = FloorPlanes::new(0, 0, f32::NAN, f32::INFINITY, vec![[7u8; 16]; 1]);
        assert_eq!((short.width(), short.height(), short.top_deg(), short.eye_m(), short.bytes()), (2, 1, 0.0, 0.0, 32));
        assert_eq!(FloorPlanes::rows_for(1024, 3.2), (247, -3.1640625));
    }

    // ---- plan hdri-8 T7: the skyline meets the floor ----------------------------------------------------------------

    /// The skyline's block on (Turn 0), the band a test hands.
    fn skyline_on() -> SkylineParams {
        SkylineParams { on: true, seed: 3, turn_deg: 0.0, lights_by_day: 0.0, settings: Default::default() }
    }

    /// A strip `w` wide from +6 to -6 degrees, its texels by (strip u, elevation of the texel's centre, degrees).
    fn strip(w: usize, texel: impl Fn(f32, f32) -> [f32; 4]) -> Arc<skyline::SkylineBand> {
        let (top, bottom) = (6.0f32, -6.0f32);
        let h = ((top - bottom) / 360.0 * w as f32).round() as usize;
        let row = (top - bottom) / h as f32;
        let texels = (0..w * h).map(|i| texel((i % w) as f32 / w as f32 + 0.5 / w as f32, top - ((i / w) as f32 + 0.5) * row)).collect();
        Arc::new(skyline::SkylineBand::new(w, h, texels, Vec::new(), top, bottom))
    }

    /// The sea of the T7 tests: open sea north (equirect u 0.35 to 0.65), its shore `near_deg` down, land elsewhere.
    fn north_sea(near_deg: f32) -> Arc<SeaEdge> {
        Arc::new(SeaEdge::new((0..512).map(|i| {
            let u = (i as f32 + 0.5) / 512.0;
            if (0.35..0.65).contains(&u) { [0.0, near_deg.to_radians()] } else { [0.0; 2] }
        }).collect()))
    }

    /// The sea the strip hands is the floor's by angle, whatever its eye (D2, Q1): open water from the horizon down to
    /// its shore in the sea's columns (`SeaEdge::at`), a beach SEA_BEACH_M of the floor's own ground inward of it, the
    /// floor's own ground beyond and on the land's columns; under snow weather the sea stays open. The edge's columns
    /// read linearly and wrap; NaN and out-of-range edges are made safe.
    #[test]
    fn the_floors_sea_is_the_strips_by_angle() {
        let edge = SeaEdge::new(vec![[0.0, 0.1], [0.0, 0.3], [f32::NAN, 9.0]]);
        assert_eq!(edge.at(0.5 / 3.0), [0.0, 0.1]);
        assert!((edge.at(1.0 / 3.0)[1] - 0.2).abs() < 1e-6, "linear between columns");
        assert_eq!(edge.at(2.5 / 3.0), [0.0, std::f32::consts::FRAC_PI_2], "made safe");
        assert!((edge.at(0.0)[1] - edge.at(1.0)[1]).abs() < 1e-6, "wraps");
        assert_eq!(edge.texels().len(), 6);
        for (weather, kind) in [(None, "grass"), (Some("snow"), "grass"), (None, "road"), (None, "lake")] {
            let mut p = with_floor(&HdriParams::default(), Some(floor_of(kind)));
            p.skyline = Some(skyline_on());
            p.sky.weather = weather.map(|kind| WeatherParams { kind: kind.to_string(), ..WeatherParams::default() });
            let images = ImageSet { skyline_sea: Some(north_sea(5.0)), ..ImageSet::default() };
            let env = Env::new_with(&p, &images);
            let model = env.floor.as_ref().unwrap();
            assert!(model.sea().is_some() && model.mirrors() && model.rippled(), "{kind}: the sea is carried on");
            let at = |az: f32, el: f32| model.sample(dir_from_az_el(az, el), 0.0);
            // (a Lake's own water lies in front, north: the sea's columns there are its water whatever)
            let sea_az = if kind == "lake" { 160.0 } else { 0.0 };
            let (sea_az, sea_u) = (sea_az, 0.5 + sea_az / 360.0);
            if kind == "lake" {
                // the shore behind a Lake: move the sea there
                let images = ImageSet { skyline_sea: Some(Arc::new(SeaEdge::new((0..512).map(|i| {
                    let u = (i as f32 + 0.5) / 512.0;
                    if (sea_u - 0.05..sea_u + 0.05).contains(&u) { [0.0, 5f32.to_radians()] } else { [0.0; 2] }
                }).collect()))), ..ImageSet::default() };
                let env = Env::new_with(&p, &images);
                let model = env.floor.as_ref().unwrap();
                let at = |el: f32| model.sample(dir_from_az_el(sea_az, el), 0.0);
                assert!(at(-3.0).water > 0.999 && at(-7.0).water == 0.0, "{kind}: the sea over the shore sector");
                continue;
            }
            for el in [-0.2, -2.0, -4.9] {
                let far = at(sea_az, el);
                assert!(far.water > 0.999 && far.ice == 0.0, "{kind} {weather:?}: water at {el}: {far:?}");
            }
            // the shore 5 degrees down at the floor's eye of 1.7 m: 19.4 m out; the beach to 15.4 m (6.3 degrees)
            let beach = at(sea_az, -5.6);
            assert!(beach.water == 0.0 && (beach.albedo - if weather.is_some() { v3(SNOW_ALBEDO) } else { v3(FloorModel::recipe(FloorKind::Lake).albedo) }).abs().max_elem() < 1e-6, "{kind}: the beach {beach:?}");
            let inland = at(sea_az, -10.0);
            assert!(inland.water == 0.0 && inland.albedo != beach.albedo, "{kind}: the floor's own ground inland");
            assert_eq!(at(sea_az + 180.0, -2.0).water, 0.0, "{kind}: none on the land's columns");
        }
        // a Mountain takes no sea (Q4), and without the skyline no floor does
        let mut p = with_floor(&HdriParams::default(), Some(floor_of("mountain")));
        p.skyline = Some(skyline_on());
        let images = ImageSet { skyline_sea: Some(north_sea(5.0)), ..ImageSet::default() };
        assert!(Env::new_with(&p, &images).floor.as_ref().unwrap().sea().is_none(), "a Mountain");
        p.floor = Some(floor_of("grass"));
        p.skyline = Some(SkylineParams { on: false, ..skyline_on() });
        assert!(Env::new_with(&p, &images).floor.as_ref().unwrap().sea().is_none(), "the skyline off");
    }

    /// Plan hdri-8 §3.10 (D2): over the sea's columns the floor's water carries the strip's sea on below the band
    /// without a line. The strip here is the sea as the floor draws it (each texel the floorless-of-strip map's own water
    /// at its centre) and clear over the land's columns, as a strip that gives way to a floor draws them. Over the sea,
    /// the map's step across the band's bottom (1e-3 degrees above it against as far below) is the floor's own step
    /// there within 1e-4 of the water's light (the skirt test's bound: no skirt over the floor, the band's lowest degree
    /// fading into the same water), and below the band the map is the floor's alone, bit for bit, its water rippled.
    /// Without a floor the same strip's skirt shows below it (the premise).
    #[test]
    fn the_floors_water_meets_the_strips_sea() {
        let mut p = sun_floor(30.0, "grass");
        p.skyline = Some(skyline_on());
        let sea = north_sea(12.0);
        let images = ImageSet { skyline_sea: Some(sea.clone()), ..ImageSet::default() };
        let alone = Env::new_with(&p, &images);
        let water = |d: Vec3f| alone.base_under_skyline(d, None, Sampling::default());
        let band = strip(1024, |u, el| {
            if el >= 0.0 {
                [0.0; 4]
            } else if sea.at(u)[1] > 0.0 {
                let c = water(dir_at(u, el));
                [c.x, c.y, c.z, 1.0]
            } else {
                [0.0; 4]
            }
        });
        let images = ImageSet { skyline: Some(band.clone()), skyline_sea: Some(sea.clone()), ..ImageSet::default() };
        let env = Env::new_with(&p, &images);
        let eps = 1e-3;
        let (mut worst, mut n) = (0.0f32, 0);
        for i in 0..64 {
            let u = 0.37 + 0.26 * i as f32 / 63.0;
            let (above, below) = (dir_at(u, band.bottom_deg + eps), dir_at(u, band.bottom_deg - eps));
            let lum = |c: Vec3f| 0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z;
            let (map_step, floor_step) = (lum(env.radiance(above)) - lum(env.radiance(below)), lum(alone.radiance(above)) - lum(alone.radiance(below)));
            worst = worst.max((map_step - floor_step).abs() / lum(alone.radiance(below)));
            n += 1;
            for el in [band.bottom_deg - 0.01, band.bottom_deg - 1.0, band.bottom_deg - 3.0] {
                let d = dir_at(u, el);
                assert!(env.radiance(d) == alone.radiance(d), "u {u}, {el}: below the band the floor alone");
                assert!(env.floor.as_ref().unwrap().sample(d, 0.0).water > 0.999, "u {u}, {el}: its water");
            }
        }
        println!("{n} sea columns: the step across the band's bottom is the floor's own within {worst:.2e} of its light");
        assert!(worst < 1e-4, "{worst}");
        // the premise: a floorless map runs the strip on in its skirt
        let mut bare = p.clone();
        bare.floor = None;
        let (with, without) = (Env::new_with(&bare, &images), Env::new_with(&bare, &ImageSet { skyline_sea: Some(sea), ..ImageSet::default() }));
        let d = dir_at(0.5, band.bottom_deg - 1.0);
        assert!(with.radiance(d) != without.radiance(d), "the floorless skirt");
    }

    /// The direction at equirect `u` and elevation `el_deg` (a bake's mapping).
    fn dir_at(u: f32, el_deg: f32) -> Vec3f {
        vec(makepad_render_material::ibl::equirect_uv_to_dir([u, 0.5 - el_deg / 180.0]))
    }

    /// Plan hdri-8 §3.10 (Q2): the strip leaves all of its flat ground to the floor, and the floor shows under it as it
    /// is, its own puddles in it, dry or wet: a strip texel below the horizon lies over the floor by its own coverage,
    /// never scaled into the share it left (the wet strip's rule without a floor, `under_band`). The strip's light hands
    /// the floor (`SkylineLight::floor`) only with one.
    #[test]
    fn a_floor_shows_under_the_strips_flat_ground() {
        let band = strip(512, |u, el| if el >= 0.0 { [0.02, 0.02, 0.02, 1.0] } else if (u * 16.0) as usize % 2 == 0 { [0.01, 0.012, 0.008, 0.3] } else { [0.0; 4] });
        for wet in [None, Some(0.7)] {
            let mut p = sun_floor(30.0, "rock");
            p.skyline = Some(skyline_on());
            p.sky.weather = wet.map(|wet_ground| WeatherParams { kind: "rain".to_string(), wet_ground, ..WeatherParams::default() });
            let env = Env::new_with(&p, &ImageSet { skyline: Some(band.clone()), ..ImageSet::default() });
            assert!(env.skyline_light().floor && !Env::new(&HdriParams { floor: None, ..p.clone() }).skyline_light().floor);
            let (mut worst, mut n) = (0.0f32, 0);
            for i in 0..96 {
                let u = (i as f32 + 0.5) / 96.0;
                for el in [-0.8, -2.0, -4.5] {
                    let d = dir_at(u, el);
                    let s = band.sample(d, 0.0);
                    // (the floor as it is under this strip: its puddles mirror the strip standing over them)
                    let base = env.base_under_skyline(d, None, Sampling::default());
                    let want = match &env.weather {
                        Some(weather) => skyline::over_veiled(s, base, &|x| env.city_veil(weather, d, None, x, 0.0)),
                        None => skyline::over_veiled(s, base, &|x| x),
                    };
                    let got = env.base_at(d, None, Sampling::default());
                    worst = worst.max((got - want).abs().max_elem() / want.max_elem().max(1e-6));
                    n += 1;
                }
            }
            println!("wet {wet:?}: {n} looks under the strip, the floor as it is within {worst:.1e}");
            assert!(worst < 1e-5, "{wet:?}: {worst}");
        }
    }

    /// Plan hdri-8 Q3: with the skyline the Lake's water runs on under the strip to the horizon (Far shore applies only
    /// without the skyline, and the GPU's ask then takes its farthest), and without a weather it mirrors the strip
    /// standing over it (the city in the lake).
    #[test]
    fn the_lake_runs_to_the_horizon_under_the_strip() {
        let mut p = sun_floor(30.0, "lake");
        p.floor.as_mut().unwrap().far_shore_m = 50.0;
        let without = Env::new(&p);
        let model = without.floor.as_ref().unwrap();
        let ahead = |model: &FloorModel| model.sample(dir_from_az_el(0.0, -0.5), 0.0);
        assert!(ahead(model).water == 0.0 && model.ask_numbers()[0] == 50.0, "without the skyline the far bank lies past 50 m");
        p.skyline = Some(skyline_on());
        let band = strip(512, |u, el| if el >= 0.0 && el < 4.0 && (u - 0.5).abs() < 0.1 { [20.0, 16.0, 10.0, 1.0] } else { [0.0; 4] });
        let env = Env::new_with(&p, &ImageSet { skyline: Some(band), ..ImageSet::default() });
        let model = env.floor.as_ref().unwrap();
        assert!(ahead(model).water > 0.999 && model.ask_numbers()[0] == FLOOR_FAR_SHORE_RANGE.1, "under the skyline the water runs to the horizon");
        let bare = Env::new(&p);
        let d = dir_from_az_el(0.0, -2.0);
        let (city, sky) = (env.radiance(d), bare.radiance(d));
        assert!(city.max_elem() > 1.2 * sky.max_elem(), "the lit city mirrored in the clear lake: {city:?} against the sky's {sky:?}");
    }

    /// Plan hdri-8 Q4: a Mountain's valley takes everything below the horizon from the strip, its sea and lakes too: the
    /// strip keeps only what stands above it. Below the horizon the map is the floor's alone (a strip opaque there,
    /// as its sea would be), above it the strip's.
    #[test]
    fn the_mountain_takes_all_below_the_horizon() {
        let mut p = sun_floor(30.0, "mountain");
        p.skyline = Some(skyline_on());
        let band = strip(512, |_, el| if el >= 0.0 { [0.03, 0.03, 0.04, 1.0] } else { [0.05, 0.1, 0.2, 1.0] });
        let images = ImageSet { skyline: Some(band.clone()), skyline_sea: Some(north_sea(5.0)), ..ImageSet::default() };
        let env = Env::new_with(&p, &images);
        let alone = Env::new(&p);
        for i in 0..64 {
            let u = (i as f32 + 0.5) / 64.0;
            for el in [-0.05, -1.0, -5.9, -7.0] {
                let d = dir_at(u, el);
                assert!(env.radiance(d) == alone.radiance(d), "u {u}, {el}: the valley");
            }
            let up = dir_at(u, 3.0);
            assert!((env.radiance(up) - vec3f(0.03, 0.03, 0.04) * env.scale()).abs().max_elem() < 1e-6, "u {u}: the strip above");
        }
    }

    /// Plan hdri-8 §6 with the skyline: without the skyline (no block, or its block off) a sea handed with the images
    /// changes nothing, and with the skyline a floor without a sea (all land) bakes as the floor under the skyline did
    /// before T7 but for the strip's give-way: every bake of these is bit-identical to the one without the sea.
    #[test]
    fn the_skyline_off_leaves_the_floor_as_it_was() {
        for kind in ["grass", "lake", "road"] {
            for block in [None, Some(SkylineParams { on: false, ..skyline_on() })] {
                let mut p = sun_floor(30.0, kind);
                p.floor.as_mut().unwrap().far_shore_m = 80.0;
                p.skyline = block;
                let plain = Env::new(&p);
                let with_sea = Env::new_with(&p, &ImageSet { skyline_sea: Some(north_sea(8.0)), ..ImageSet::default() });
                assert!(with_sea.floor.as_ref().unwrap().sea().is_none() && !with_sea.floor.as_ref().unwrap().under_skyline());
                assert!(plain.bake(128) == with_sea.bake(128), "{kind}: the sea of a skyline that is off");
                assert_eq!(plain.floor.as_ref().unwrap().ask_numbers(), p.floor.as_ref().unwrap().clamped().kind_numbers(), "{kind}: the numbers as set");
            }
        }
    }
}
