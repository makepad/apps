//! The Altocumulus volume (design §4.3; the look lanes' Altocumulus): a mid-level deck of small rounded cloudlets in
//! rows, grey sides and bright rims, blue between them (the mackerel sky). It stands on the Cumulus volume's machinery
//! (cloud_volume_cumulus.rs: the footprint grid, the sun-depth grid, Cover's solve, the CPU march as the fallback)
//! through its own footprint recipe, its own shell and the rows mapped onto the shared shape's; on the GPU it has its
//! own density (the shader's `// --- Altocumulus ---` section), whose numbers go up as the `ac*` uniforms.
//!
//! - **The cloudlets** ([`lattice`]): one a cell of Cell size, every other line of cells half a cell over (packed as
//!   cells pack), at random in its cell as far as (1 - Regularity) lets it (a lattice at 1), each with its own reach
//!   and strength. A point belongs to its nearest cloudlet (by distance over reach: a weighted Voronoi); the field is
//!   1 - (d1 / d2)^CELL_POWER times that cloudlet's strength, 1 at its middle and 0 at the border with the next nearest,
//!   so the threshold cuts lanes along the borders: cloudlets touching in rolls with narrow lanes of blue at a high
//!   Cover, shrinking to round puffs at a low one. Rows stretches the cells across the wind and takes their stagger
//!   away, so the cloudlets line up in bars across it (the mackerel sky's billows). Each has its own height (the column
//!   channel). The cloudlets are a few hundred metres wide, a few footprint-grid texels (66 m), so the GPU evaluates
//!   them itself at full resolution, integer for integer as here; the grid holds what is smooth.
//! - **The deck** (the grid's strength channel, `m`: the strongest a cloudlet can be there): rafts some 1 / PATCH_FREQ
//!   cells wide with clear sky between, and Rows' bands across the wind, bent by a slow field (undulatus). The
//!   footprint is the strongest bump, over a floor, times `m`.
//! - **The shell**: from Base up DEPTH_PER_CELL of a cell (flat cloudlets, a quarter to a third as tall as wide),
//!   TURRET_RISE times more at Turrets 1, where some cloudlets rise into towers (castellanus).
//! - **On the GPU** each cloudlet is a flat soft lens: a flat underside, a rounded top, its density fading over a share
//!   of the cell set by Softness (no hard surface, no rim), mottled by the billow tile. Past the footprint grid the deck
//!   carries on to the horizon as the far deck: its mean strength, the cloudlets at their means (`deck_mean`), marched
//!   to FAR_RANGE_M.

use makepad_draw::*;
use crate::cloud_sheets::{wave, Streams};
use crate::cloud_volume::Node;
use crate::noise::{fbm2, hash_u32};
use crate::AltocumulusParams;

/// The deck's depth over its cell: the photos' cloudlets are flat, a quarter to a third as tall as wide (at 0.8 they
/// stood as tall as wide, small heaped cumulus puffs), never under DEPTH_LEAST_M; Turrets raises the shell's top by up
/// to TURRET_RISE of that (castellanus' towers stand several times their cell's height; at 2 they read as bumps).
pub const DEPTH_PER_CELL: f32 = 0.3;
pub const DEPTH_LEAST_M: f32 = 60.0;
/// How far the far deck is marched (metres along the ground, the march's range for an Altocumulus): past the
/// footprint grid's 34 km the deck carries on at its mean, so it meets the horizon's haze instead of ending at el 6.
pub const FAR_RANGE_M: f32 = 180_000.0;
/// The grid's deck blends into the far deck from FAR_BLEND_M.0 to .1 metres away (the grid ends at 34 km).
const FAR_BLEND_M: (f32, f32) = (22_000.0, 32_000.0);
/// The veil: the lanes keep VEIL of a cell's density (times the share of plane the cells cover) in a slab from the
/// deck's base up VEIL_TOP of the deck (paler lanes, the photos' thin veil between the cells), where the deck is
/// (its strength from VEIL_DECK.0 to .1: the holes between the patches stay clear blue) and round a cloudlet (its own
/// strength from VEIL_DECK.2 to .3: a missing cloudlet's place stays clear, not a grey smudge a cell wide).
const VEIL: f32 = 0.1;
const VEIL_TOP: f32 = 0.4;
const VEIL_DECK: (f32, f32, f32, f32) = (0.2, 0.6, 0.05, 0.3);
/// The lens (the GPU's density, and `lens_density`, its CPU twin that Cover's views read):
/// - EDGE_SPAN: its density and height rise over EDGE_SPAN.0 + EDGE_SPAN.1 x Softness of the footprint over the
///   threshold (a crisp lens at Softness 0, a soft tuft at 1);
/// - TOP_POWER, DENSITY_POWER: its top rises as that edge to TOP_POWER (a rounded rim, no wall), its density as the edge
///   to DENSITY_POWER (no bright line at the rim);
/// - TOP_SKIN: its density rises over TOP_SKIN.0 + TOP_SKIN.1 x Softness of its height under its top, BASE_SKIN of that
///   over its underside (a flat, darker base);
/// - UNDERSIDE_LIFT: its underside lifts toward the rim by that share of its height;
/// - DECK_SHARE: a cloudlet's top as a share of the deck, DECK_SHARE.0 + DECK_SHARE.1 x its column;
/// - DENSITY: its density against a Cumulus' on the GPU (a thin cloud: an optical depth of 0.3 to 1 along a view
///   through a cloudlet, the sky showing through it, as through the real 105_1423's; at 0.2 it was white cotton), and
///   COVER_DENSITY the one Cover's views count it with, so Cover is about the share of sky the visible cloud stands
///   in (counted at 0.2, Cover 0.6 showed far less cloud than the real sky has at that share; counted at the thin
///   density, the solve would close a translucent deck until it hid that share);
/// - MOTTLE: the billow tile read at MOTTLE.0 cells takes up to MOTTLE.1 of the density away and lifts the top by up to
///   MOTTLE.2 (its mean taking MOTTLE_MEAN away, what the twin reads).
const EDGE_SPAN: (f32, f32) = (0.1, 0.6);
/// A cloudlet's edge is at most OWN_EDGE times its own rise over the threshold (its peak's), never under EDGE_LEAST: a
/// weak cloudlet is small and solid, not a faint wisp (at a tiny Cover only the strongest tips pass).
const OWN_EDGE: f32 = 1.0;
const EDGE_LEAST: f32 = 0.01;
const TOP_POWER: f32 = 0.75;
const DENSITY_POWER: f32 = 1.0;
const TOP_SKIN: (f32, f32) = (0.25, 0.5);
const BASE_SKIN: f32 = 0.35;
const UNDERSIDE_LIFT: f32 = 0.25;
const DECK_SHARE: (f32, f32) = (0.62, 0.3);
const DENSITY: f32 = 0.06;
const COVER_DENSITY: f32 = 0.13;
/// A turret's core density against the lens', and the cell's own profile over which a turret stands (narrow; at 0.8 the
/// towers were a few texels wide and read as dithered ghosts).
const TOWER_DENSITY: f32 = 2.0;
const TOWER_PROFILE: f32 = 0.65;
const MOTTLE: [f32; 3] = [0.6, 0.35, 0.12];
const MOTTLE_MEAN: f32 = 0.5;
/// The export's extinction scale (the shader's `xa.x`): the GPU's sigma is the shape's times it.
const EXPORT_SIGMA_SCALE: f32 = 2.0;
/// The type's light on the GPU: its shares of the light law's sun, dome, open side and bounce (0.08, 0.3, 1, 0.1 of a
/// Cumulus': the template's knobs xf.x, xe.z, xe.y and xf.z at those shares, in its export section,
/// cloud_gpu/src/export/altocumulus.rs, with its fine step at most 20 m and a fresh sun march at every sample), and its
/// forward lobe FORWARD_G (the law's G_FORWARD is 0.8): a thin deck lit through, not a Cumulus' sunlit faces. Fitted to the
/// real sky 105_1423 at its sun (89 / 54) and checked on evening_road_01 (127 / 10), cloud over gap in 10-degree
/// windows (look-hi-7.md):
/// - the sun: a droplet sends most of its forward light into a diffraction peak a few degrees wide, so 10-30 degrees
///   from the sun a thin cloud scatters far less than the Henyey-Greenstein lobes with their octaves give: at round 6's
///   0.34 the cloudlets stood 7-14 times over the gaps toward the sun and overhead (the real sky 0.5-1.1);
/// - the forward lobe: the real sky's glow falls 2.3 times from 10-15 to 25-30 degrees off the sun; HG(0.8) falls 4.7
///   times, HG(0.6) twice: the broad milky wash near the sun;
/// - the dome, open side and bounce: a forward-scattering veil turns the sky behind it toward the eye more than it
///   spreads the whole dome's light (the dome at 1 left it 1.2-1.5 times the gaps' sky away from the sun, where the
///   real one is as dark as its sky), and sends the ground's light back down little.
const FORWARD_G: f32 = 0.6;
pub const TURRET_RISE: f32 = 4.0;
/// A cloudlet's reach (cells): REACH times its own share from REACH_LEAST to all (at Regularity 0; toward 1 the share
/// varies less, as (1 - Regularity^2), so a lattice is of equal cloudlets). The discs overlap, so the plane is
/// covered at a low threshold and Cover 0.5 cuts the bumps half-way: there the deck's patches decide where the sky is
/// clear, and the blue between cloudlets is a net of lanes. A cloudlet two cells off is at least a cell away, so the
/// 3 x 3 search finds every bump while REACH stays under 1 (Rows' stretch is in the cells, not the reach).
const REACH: f32 = 0.85;
const REACH_LEAST: f32 = 0.55;
/// The cell's profile, 1 - (d1 / d2)^CELL_POWER: falling to 0 at the border with the next cloudlet, so a lane follows
/// every border (round 3's peaked bumps, (1 - d^2)^2, left separate puffs; flat-topped bumps merged into rafts with no
/// lanes). At 3 the profile fell so steeply at the border that a soft edge set over the footprint was a few metres
/// wide: crisp, stepped rims.
const CELL_POWER: f32 = 1.5;
/// Its strength from STRENGTH_LEAST to all (at Regularity 0; at 1 from STRENGTH_EVEN of the way there).
const STRENGTH_LEAST: f32 = 0.3;
/// A share of the cloudlets is missing (MISSING_BYTE of 256: a hole a cell wide), and a share of the borders between two
/// cloudlets of one column (neighbours across the wind) is shallow (MERGE_BYTE of 256: the crease keeps 1 - MERGE_FILL
/// of its depth, and the two merge into one body: the rolls across the wind; merged in any direction they drew worms);
/// the merge fades to a full lane where a third cloudlet is within MERGE_JUNCTION (over reach) as near as the second,
/// so the junctions keep no seam.
const MISSING_BYTE: u32 = 26;
const MERGE_BYTE: u32 = 140;
const MERGE_FILL: f32 = 0.8;
const MERGE_JUNCTION: f32 = 0.15;
const STRENGTH_EVEN: f32 = 0.3;
/// At Regularity 0 a cloudlet stands anywhere in its cell (JITTER of it round its middle).
const JITTER: f32 = 1.0;
/// The domain's warp: WARP cells at WARP_FREQ cycles a cell (two octaves), so no lattice is ruled.
const WARP: f32 = 0.2;
const WARP_FREQ: f32 = 0.3;
/// The patches: an fbm at PATCH_FREQ cycles a cell (rafts some 3.5 km across at the default Cell size, the real
/// 105_1423's broken sheet; at 0.1 one raft filled an overhead view) over PATCH_EDGE mapped to 0..1; where it
/// is 0 the cloudlets keep PATCH_LEAST of their strength, and the sky between the rafts is clear.
const PATCH_FREQ: f32 = 0.14;
const PATCH_EDGE: (f32, f32) = (0.35, 0.65);
const PATCH_LEAST: f32 = 0.1;
/// Between the cloudlets the footprint keeps FLOOR of the deck: inside a strong raft the lanes pass the threshold as
/// thin cloud, so a raft is a soft sheet with its cells as texture in it (the real sky's), while between rafts the
/// lanes stay open; Cover 1 closes the sky. At 0.05 every lane was blue: a print of separate cells.
const FLOOR: f32 = 0.3;
/// Rows: the cells stretch across the wind by up to ROW_STRETCH more (a cloudlet a bar across it) and lose their
/// stagger; bands across the wind ROW_PERIOD cells apart, bent by up to ROW_BEND periods over ROW_BEND_FREQ cycles a
/// cell, fade the bars between them by up to ROW_DEPTH (every other bar weaker: the billows' rhythm).
const ROW_STRETCH: f32 = 3.0;
const ROW_PERIOD: f32 = 2.0;
const ROW_BEND: f32 = 0.25;
const ROW_BEND_FREQ: f32 = 0.04;
const ROW_DEPTH: f32 = 0.5;
/// The column channel the shared shape reads: COLUMN_FLOOR and up, so its tops table at Towers 1 (the shell's top at
/// every column over it) bounds the GPU's own tops, which read the cloudlet's column itself.
const COLUMN_FLOOR: f32 = 0.3;
/// Cell indices are moved this far up before their hash, so no index is negative (the shader's u32 of it).
const CELL_OFFSET: i32 = 1 << 20;
/// The recipe's id in the footprint grid's key (the Cumulus' is 3).
pub const RECIPE_ID: f32 = 11.0;

/// The Altocumulus shell (base, top), metres.
pub fn shell_m(b: &AltocumulusParams) -> (f32, f32) {
    let depth = (b.cell_m * DEPTH_PER_CELL).max(DEPTH_LEAST_M);
    (b.base_m, b.base_m + depth * (1.0 + TURRET_RISE * b.turrets.clamp(0.0, 1.0)))
}

/// What the shared shape reads of an Altocumulus (cloud_volume_cumulus.rs `VolumeRows`), the CPU fallback's stand-in:
/// its Cell size as the Size, its shell, Towers 1 (its tops table the bound of the GPU's own tops), rounded flattened
/// cloudlets (a softer base than a Cumulus', some puff), soft edges (Softness 0.6: at 0.3 a cell's edge crossing the
/// sun's disc left the fallback's disc texels 4 % off the key's cone mean), no lean, the Cumulus' density (it has no
/// Density row). The GPU's look reads none of these but the shell.
pub struct Rows {
    pub size_km: f32,
    pub base_m: f32,
    pub tops_m: f32,
    pub towers: f32,
    pub flat_base: f32,
    pub puffiness: f32,
    pub softness: f32,
    pub density: f32,
}

pub fn rows(b: &AltocumulusParams) -> Rows {
    let (base_m, tops_m) = shell_m(b);
    Rows { size_km: b.cell_m * 1.0e-3, base_m, tops_m, towers: 1.0, flat_base: 0.35, puffiness: 0.2, softness: 0.6, density: 2.0 }
}

/// The cloudlets' layout: the jitter (cells), the cells' stretch across the wind, every other line's stagger (cells),
/// the least reach (a share of REACH) and the least strength.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub jitter: f32,
    pub stretch: f32,
    pub stagger: f32,
    pub reach_least: f32,
    pub strength_least: f32,
}

impl Layout {
    pub fn of(regularity: f32, rows: f32) -> Layout {
        let (regularity, rows) = (regularity.clamp(0.0, 1.0), rows.clamp(0.0, 1.0));
        let vary = 1.0 - regularity * regularity;
        Layout {
            jitter: JITTER * (1.0 - regularity * regularity),
            stretch: 1.0 + ROW_STRETCH * rows,
            stagger: 0.5 * (1.0 - rows),
            reach_least: 1.0 - (1.0 - REACH_LEAST) * vary,
            strength_least: 1.0 - (1.0 - STRENGTH_LEAST) * (STRENGTH_EVEN + (1.0 - STRENGTH_EVEN) * vary),
        }
    }
}

/// The cloudlets round `fa` (wind frame, cells, shifted as the recipe's), of the lattice `seed`: [the cell's value at
/// the point (its profile times its cloudlet's strength), that cloudlet's strength, its column (its own value), the
/// profile alone]. The nearest two cloudlets are found among the 3 x 3 cells round the point (by distance over reach).
/// The shader's `aclattice` is the same, integer for integer.
pub fn lattice(fa: Vec2f, seed: u32, k: Layout) -> [f32; 4] {
    let fa = vec2f(fa.x, fa.y / k.stretch);
    let iy = fa.y.floor() as i32;
    // the three nearest cloudlets by distance over reach, and the nearest two's hashes
    let (mut d1, mut d2, mut d3, mut h1, mut h2, mut x1, mut x2) = (f32::INFINITY, f32::INFINITY, f32::INFINITY, 0u32, 0u32, 0i32, 0i32);
    for cy in iy - 1..=iy + 1 {
        let hy = hash_u32(seed ^ ((cy + CELL_OFFSET) as u32).wrapping_mul(0x9e37_79b9));
        // every other line of cells over by the stagger, and the search along it round the point's own cell there
        let stagger = k.stagger * ((cy + CELL_OFFSET) & 1) as f32;
        let jx = (fa.x - stagger).floor() as i32;
        for cx in jx - 1..=jx + 1 {
            let h = hash_u32(hy ^ ((cx + CELL_OFFSET) as u32).wrapping_mul(0x85eb_ca6b));
            let byte = |at: u32| ((h >> at) & 255) as f32 / 255.0;
            let ox = cx as f32 + stagger + 0.5 + k.jitter * (byte(0) - 0.5);
            let oy = cy as f32 + 0.5 + k.jitter * (byte(8) - 0.5);
            let reach = REACH * (k.reach_least + (1.0 - k.reach_least) * byte(16));
            let (dx, dy) = ((fa.x - ox) / reach, (fa.y - oy) / reach);
            let d = (dx * dx + dy * dy).sqrt();
            if d < d1 {
                (d3, d2, d1) = (d2, d1, d);
                (h2, h1) = (h1, h);
                (x2, x1) = (x1, cx);
            } else if d < d2 {
                (d3, d2) = (d2, d);
                h2 = h;
                x2 = cx;
            } else if d < d3 {
                d3 = d;
            }
        }
    }
    // each cloudlet's strength (none for a missing one) and column
    let strength = |h: u32| if (hash_u32(h ^ 0x9e37_79b1) >> 24) < MISSING_BYTE { 0.0 } else { k.strength_least + (1.0 - k.strength_least) * ((h >> 24) & 255) as f32 / 255.0 };
    let column = |h: u32| (hash_u32(h ^ 0x27d4_eb2f) >> 8) as f32 * (1.0 / 16_777_216.0);
    let r = (d1 / d2.max(1.0e-6)).powf(CELL_POWER);
    // a merged border: shallow, and the strength and column blend across it (the same on both sides)
    let merged = if x1 == x2 && (hash_u32(h1 ^ h2) >> 24) < MERGE_BYTE { smooth(0.0, MERGE_JUNCTION, d3 - d2) } else { 0.0 };
    let profile = 1.0 - r * (1.0 - MERGE_FILL * merged);
    let blend = 0.5 * merged * r;
    let (s1, s2) = (strength(h1), strength(h2));
    let s = s1 + (s2 - s1) * blend;
    let c = column(h1) + (column(h2) - column(h1)) * blend;
    [profile * s, s, c, profile]
}

/// The Altocumulus footprint recipe: its rows that change the footprint (`fields`), Turrets and Softness (the GPU's
/// tops and edges), the cloudlets' means (the GPU's far blend) and the deck's mean strength (the far deck), measured
/// once.
#[derive(Clone, Copy, Debug)]
pub struct AltocumulusRecipe {
    pub regularity: f32,
    pub rows: f32,
    pub turrets: f32,
    pub softness: f32,
    /// The strongest bump's mean over the plane and its cloudlet's mean strength where there is one.
    pub means: [f32; 2],
    /// The deck's mean strength over the plane (`m`, the grid's strength channel).
    pub deck_mean: f32,
}

impl AltocumulusRecipe {
    pub fn new(b: &AltocumulusParams) -> AltocumulusRecipe {
        let k = Layout::of(b.regularity, b.rows);
        // 64 x 64 points over 16 x 16 cells of a fixed lattice: the means are the layout's, not the seed's.
        let (mut bump, mut peak, mut peaks) = (0.0f32, 0.0f32, 0.0f32);
        for j in 0..64 {
            for i in 0..64 {
                let l = lattice(vec2f((i as f32 + 0.5) * 0.25, (j as f32 + 0.5) * 0.25), 0x5bd1_e995, k);
                bump += l[0];
                if l[1] > 0.0 {
                    peak += l[1];
                    peaks += 1.0;
                }
            }
        }
        let mut recipe = AltocumulusRecipe { regularity: b.regularity, rows: b.rows, turrets: b.turrets, softness: b.softness, means: [bump / 4096.0, if peaks > 0.0 { peak / peaks } else { 1.0 }], deck_mean: 1.0 };
        // The deck's mean: 64 x 64 points over 256 x 256 cells of the measuring streams (some 20 rafts a side).
        let streams = Streams::of(crate::cloud_cover::MEASURE_SEED);
        let mut deck = 0.0f32;
        for j in 0..64 {
            for i in 0..64 {
                deck += recipe.at(vec2f((i as f32 + 0.5) * 4.0, (j as f32 + 0.5) * 4.0), &streams)[4];
            }
        }
        recipe.deck_mean = deck / 4096.0;
        recipe
    }

    /// The shift of the stream `s` (cells along and across) and the lattice's seed.
    fn shift(s: &Streams) -> (Vec2f, u32) {
        let unit = |salt: u32| (hash_u32(s.0[3] ^ salt) >> 8) as f32 * (1.0 / 16_777_216.0);
        (vec2f(unit(0x3c6e_f372), unit(0xa54f_f53a)), s.0[4])
    }

    /// The footprint's noise (the strongest cloudlet's bump over FLOOR, times the deck), its column channel, its domain warp (cells along
    /// and across) and the deck `m` (the strength channel: the strongest a cloudlet can be here) at `f` (wind frame,
    /// cells).
    pub fn at(&self, f: Vec2f, s: &Streams) -> Node {
        self.sample(f, s).0
    }

    /// `at`, the footprint at the middle of the cloudlet `f` belongs to (its peak: the deck times its strength over the
    /// floor), which its soft edge is measured against, and that cloudlet's strength.
    pub fn sample(&self, f: Vec2f, s: &Streams) -> (Node, f32, f32) {
        let (shift, seed) = Self::shift(s);
        let f = f + shift;
        let wx = fbm2(f.x * WARP_FREQ + 4.7, f.y * WARP_FREQ + 2.3, 2, s.0[5]) - 0.5;
        let wz = fbm2(f.x * WARP_FREQ + 8.1, f.y * WARP_FREQ + 6.6, 2, s.0[6]) - 0.5;
        let w = vec2f(f.x + 2.0 * WARP * wx, f.y + 2.0 * WARP * wz);
        let patch = smooth(PATCH_EDGE.0, PATCH_EDGE.1, fbm2(f.x * PATCH_FREQ + 1.3, f.y * PATCH_FREQ + 7.9, 3, s.0[7]));
        let bend = 2.0 * ROW_BEND * (fbm2(w.x * ROW_BEND_FREQ + 5.2, w.y * ROW_BEND_FREQ + 0.4, 2, s.0[2]) - 0.5);
        let band = wave(w.x / ROW_PERIOD + bend);
        let m = (PATCH_LEAST + (1.0 - PATCH_LEAST) * patch) * (1.0 - self.rows.clamp(0.0, 1.0) * ROW_DEPTH * (1.0 - band));
        let l = lattice(w, seed, Layout::of(self.regularity, self.rows));
        let node = [((FLOOR + (1.0 - FLOOR) * l[0]) * m).clamp(0.0, 1.0), COLUMN_FLOOR + (1.0 - COLUMN_FLOOR) * l[2], w.x - f.x, w.y - f.y, m];
        (node, ((FLOOR + (1.0 - FLOOR) * l[1]) * m).clamp(0.0, 1.0), l[1])
    }

    /// The fields its quantile tables are measured for (design §3.1's fingerprint).
    pub fn fields(&self) -> [f32; 2] {
        [self.regularity, self.rows]
    }
}

/// A lens' point: its footprint `n`, its cloudlet's `peak` (`AltocumulusRecipe::sample`) and `column`, the cover gate
/// `f` there, and its height `h01` (0..1 through the shell).
pub struct LensPoint {
    pub n: f32,
    /// The deck's strength there (the recipe's `m`) and its cloudlet's (blended across a merged border).
    pub deck: f32,
    pub strength: f32,
    pub peak: f32,
    pub column: f32,
    pub f: f32,
    pub h01: f32,
}

/// The lens' extinction (per metre) at the point `at`, at a `threshold`, in a shell `thickness_m` deep and of the
/// shape's `sigma`: the CPU twin of the shader's `acdens` without its mottling (its mean instead), its turrets and its
/// far deck, at an exact footprint. Cover's views read it (`VolumeShape::sky_opacity`), so the share of sky they count
/// hidden is the look's.
pub fn lens_density(r: &AltocumulusRecipe, at: &LensPoint, threshold: f32, share: f32, thickness_m: f32, sigma: f32) -> f32 {
    let LensPoint { n, peak, column, f, h01, .. } = *at;
    let softness = r.softness.clamp(0.0, 1.0);
    let span = (OWN_EDGE * (peak - threshold)).clamp(EDGE_LEAST, (EDGE_SPAN.0 + EDGE_SPAN.1 * softness).max(EDGE_LEAST));
    let core = if n > threshold { smooth(threshold, threshold + span, n) } else { 0.0 };
    let deck = (DECK_SHARE.0 + DECK_SHARE.1 * column) / (1.0 + TURRET_RISE * r.turrets.clamp(0.0, 1.0));
    let veil = VEIL * share.clamp(0.0, 1.0) * smooth(VEIL_DECK.0, VEIL_DECK.1, at.deck) * smooth(VEIL_DECK.2, VEIL_DECK.3, at.strength) * (1.0 - core) * if h01 < VEIL_TOP * deck { 1.0 } else { 0.0 } * COVER_DENSITY * sigma * EXPORT_SIGMA_SCALE;
    if !(core > 0.0) {
        return veil;
    }
    let top = deck * core.powf(TOP_POWER);
    let bottom = UNDERSIDE_LIFT * (1.0 - core) * top;
    if !(h01 < top && h01 > bottom) {
        return veil;
    }
    let height_m = ((top - bottom) * thickness_m).max(1.0);
    let skin_up = (TOP_SKIN.0 + TOP_SKIN.1 * softness) * height_m;
    let den_v = smooth(0.0, skin_up, (top - h01) * thickness_m) * smooth(0.0, BASE_SKIN * skin_up, (h01 - bottom) * thickness_m);
    let gate = (f * 3.0).min(1.0);
    core.powf(DENSITY_POWER) * den_v * (1.0 - MOTTLE[1] * MOTTLE_MEAN) * gate * COVER_DENSITY * sigma * EXPORT_SIGMA_SCALE + veil
}

/// The horizon fade of an Altocumulus' alpha in the compositor: its far deck reaches the horizon, so it fades over the
/// horizon line's width only (sin el HORIZON_LINE), not a flat entry's 7 degrees, which showed the clear sky's bright
/// horizon in a band under the deck.
pub fn horizon_fade(sin_el: f32) -> f32 {
    smooth(0.0, HORIZON_LINE, sin_el)
}

const HORIZON_LINE: f32 = 0.002;

fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The Altocumulus look's numbers on the GPU (the shader's `// --- Altocumulus ---` section), as the look rounds tune
/// them: the `ac*` uniforms. `share` is the share of plane the footprint covers (the Cover solve's): the far blend's.
pub(crate) fn gpu_layout(out: &mut crate::cloud_volume_gpu::UploadBuilder, r: &AltocumulusRecipe, s: &Streams, share: f32) {
    let k = Layout::of(r.regularity, r.rows);
    let (shift, seed) = AltocumulusRecipe::shift(s);
    // ac_b: the underside's lift toward the rim (a share of the cloudlet's height: a flat base that curls up at its
    // edge)
    out.uniform("ac_b", [UNDERSIDE_LIFT, 0.0, 0.0, 0.0]);
    // ac_c: a cloudlet's top as a share of the deck's depth (from x at column 0 by y to column 1), the share of the
    // cloudlets that rise into turrets, how far they rise (TURRET_RISE x Turrets: the shell's growth)
    out.uniform("ac_c", [DECK_SHARE.0, DECK_SHARE.1, 0.3, TURRET_RISE * r.turrets.clamp(0.0, 1.0)]);
    // ac_d: the far blend's start and end (z, w: the texel's footprint over the cell; from a tenth, ten texels a cell:
    // from a thirtieth the 1K preview's lanes held a grey veil overhead)
    out.uniform("ac_d", [0.0, 0.0, 0.1, 0.4]);
    // ac_e: the density against a Cumulus' (y: thin, a soft grey gradient through a cloudlet), the cell's own profile
    // over which a turret stands (z: a narrow tower over the middle of its cell)
    out.uniform("ac_e", [0.0, DENSITY, TOWER_PROFILE, 0.0]);
    // its forward lobe (the law's own k_light, replaced for this volume alone)
    crate::cloud_volume_light::gpu_forward_lobe(out, FORWARD_G);
    // ac_q: the least strength, the cell value's mean and the mean strength (the far blend's lattice)
    out.uniform("ac_q", [k.strength_least, r.means[0], r.means[1], 0.0]);
    // ac_f: the footprint's floor between the cloudlets, every other line's stagger (cells), the cell's profile power;
    // ac_t: the missing cloudlets' and the merged borders' shares (of 256), a merged crease's fill, the junction's fade
    out.uniform("ac_f", [FLOOR, k.stagger, CELL_POWER, 0.0]);
    out.uniform("ac_t", [MISSING_BYTE as f32, MERGE_BYTE as f32, MERGE_FILL, MERGE_JUNCTION]);
    // ac_g: the cloudlets' outlines: how far the shape tile moves the lattice's point (cells), its frequency (cycles a
    // cell)
    out.uniform("ac_g", [0.2, 1.0, 0.0, 0.0]);
    // ac_k: the soft edge: a cloudlet's density and height rise over this much of the footprint over the threshold (x:
    // 0.05 at Softness 0, a crisp lens, to 0.55 at 1, a soft tuft), its top's skin as a share of its height (y), the
    // top's rise and the density's over that edge as powers of it (z, w: a rounded rim with no wall, the density fading
    // with no bright line)
    let softness = r.softness.clamp(0.0, 1.0);
    out.uniform("ac_k", [EDGE_SPAN.0 + EDGE_SPAN.1 * softness, TOP_SKIN.0 + TOP_SKIN.1 * softness, TOP_POWER, DENSITY_POWER]);
    // ac_l: a cloudlet's own edge (OWN_EDGE times its rise over the threshold, never under EDGE_LEAST), the veil (its
    // density against a cell's, times the share; its top as a share of the deck); ac_v: the deck's strength over which
    // the veil stands
    out.uniform("ac_l", [OWN_EDGE, EDGE_LEAST, VEIL, VEIL_TOP]);
    out.uniform("ac_v", [VEIL_DECK.0, VEIL_DECK.1, VEIL_DECK.2, VEIL_DECK.3]);
    // ac_w: -, a turret's core density (times the lens')
    out.uniform("ac_w", [0.0, TOWER_DENSITY, 0.0, 0.0]);
    // ac_m: the mottling: the billow tile read at x cells (its largest balls), how much of the density it takes away (y),
    // how high it lifts the top (z, a share of the deck); the underside's skin as a share of the top's (w)
    out.uniform("ac_m", [MOTTLE[0], MOTTLE[1], MOTTLE[2], BASE_SKIN]);
    // ac_x: the far deck: the deck's mean strength, the blend into it from y to z metres away, the share of plane the
    // cloudlets cover (the far blend's gate); k_earth.z its range
    out.uniform("ac_x", [r.deck_mean, FAR_BLEND_M.0, FAR_BLEND_M.1, share.clamp(0.0, 1.0)]);
    out.uniform("k_earth", [crate::volumetric::EYE_M, crate::volumetric::PLANET_M, FAR_RANGE_M, crate::volumetric::MIN_TRANSMITTANCE]);
    // ac_r: the jitter (cells), the reach (cells), its least share, the cells' stretch across the wind
    out.uniform("ac_r", [k.jitter, REACH, k.reach_least, k.stretch]);
    // ac_s: the stream's shift (cells along, across) and the lattice's seed in two 16-bit halves
    out.uniform("ac_s", [shift.x, shift.y, (seed & 0xffff) as f32, (seed >> 16) as f32]);
}
