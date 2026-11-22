//! The Stratocumulus as a cloud volume (design §2.2's table, §4.3): a lumpy deck in a thin shell.
//!
//! - **Footprint.** The deck's phase-1 recipe (cloud_sheets.rs's Stratocumulus): cells of one billowed octave over the
//!   broad deck fbm, Billows' lumps at a third of a cell, Rolls' warped wave across the wind, a Size apart. Its share
//!   of the plane comes from Cover's solve (design §3), as the Cumulus'. The column channel sets each part of the deck's
//!   top (0.7 to 1 of the shell, design §2.2's tops); the grid also carries a warp for the cells and the billowed
//!   octave's own noise.
//! - **The slab and its cells.** Where the footprint stands over its threshold the deck is one slab as tall as it is deep
//!   over the threshold (DOME_EDGE, rising as its DOME_POWER: round shoulders), its underside lifting toward the
//!   deck's outline (EDGE_LIFT: a lens' rim). Its cells (Voronoi cells CELL_SIZE of Size apart on the warped plane,
//!   gathered in groups by the footprint's creases) are gentle bumps on it: the top dips toward a crack by TOP_DIP of
//!   the height and the underside rises by UNDER_RISE, so seen from the side it is a layer with a bumpy underside, not a
//!   row of pillows (as pillows as tall as wide, round 3's cells read as cumulus heaps). The cells shape its faces only:
//!   a cell's density fades to its rim's (over CELL_EDGE: a good part of the cell, no hard rim) within FACE_M of a face,
//!   and the slab's middle is plain (as columns taller than wide, round 4's cells hung as drips on slant views). Soft
//!   holes a few cells wide open in it (`holes`), along its cracks first: most where the deck is shallow and as Cover
//!   falls, few where it is deep, their edges graded over some 100-200 m, where the faces close toward each other as a
//!   lens' rim (no wall) and the density thins. Each cell's height and density are its own. Billows over 0.6
//!   raises turrets from some cells (castellanus); under 0.3 the cells are drawn out along the wind (lenticularis). The
//!   distance into the deck is measured in metres up and down (a deck's faces are its top and its underside).
//! - **The balls.** The billow tile read SQUASH times finer up than across (balls four times wider than tall, design
//!   §2.4: a Stratocumulus is 4-16 times as wide as tall) lumps both faces on the GPU, the top by BALL_TOP_M a unit of
//!   the tile and the underside by BALL_BOTTOM_M, both times Billows' share: a lumpy top, a flat underside.
//! - **Far away** (FAR_FROM to FAR_TO metres) the cells close and the relief and texture fade: a smooth band into the
//!   haze, as the photographs' decks are low on the horizon.
//! - **Light.** Its density thins toward the cracks (a rim keeps exp(-RIM_K x Shading) of a cell's middle), so a
//!   cell's middle is darker below and its rim bright: Shading is the thickness contrast (design §7), no longer a
//!   self-shadow. Its export shader (cloud_gpu's export/stratocumulus.rs) holds the light law at a deck's shares and
//!   its own march: a deck is lit as a slab, the sun diffusing down through the
//!   column over a point and the dome reaching it the same way, plus the sky its gaps show (`ScShape::sky`), so its
//!   thick middles are grey and its edges by a gap bright. On the GPU part of the slab share follows the sun's lobe
//!   (SLAB_LOBE): bright toward a low sun, dark away from it.
//! - **Two densities.** The GPU export's (cloud_gpu's `scdens`) is the look: the cells' plane also wanders at their
//!   own scale and their edges are ragged, flattened balls lump the faces, faint fibres along the wind, an eroded top
//!   skin and thin patches give it texture. The CPU's (`ScShape::cheap`: the fallback march, held bakes, the Cover
//!   solve) is the same slab with its holes on plain cells, without the wander, the raggedness, the balls or the
//!   texture, and a coarser lattice
//!   (LATTICE_M): a rough deck, at a fraction of the cost. The sun's grids read the pillows at their cells' mean
//!   (`ScShape::sun_field`).

use super::*;
use crate::cloud_volume_gpu::UploadBuilder;
use crate::noise::billow2;

/// Its extinction per metre at Density 1 (design §2.2's table puts it under a Cumulus', infinigen's ranges at 0.3-0.5
/// of one), so a cell's middle is some 10-15 deep, as a Stratocumulus' water makes it (some 100 g/m2 in droplets of 10
/// microns): grey below, its thin rims and cracks bright toward the sun (aristea_wreck), a greyer deck under a low sun
/// (citrus_orchard). At 0.036 its column passed some 0.7 of the light above it and a low sun's deck stood white; at
/// 0.09 a column's opacity saturated, and the cells' depth read through their cracks alone. At 0.2 (rounds 3-4) a 300 m
/// deck was some 60 deep, its middles and its thinned cracks alike past the column's measure (the cells' signature
/// read its holes, 500-700 m), and real decks are 5-20 deep: at 0.085 a middle is some 25 deep and a crack, through the
/// top's dip, some 11 (the cells read 175 m apart again). The CPU marches it whole;
/// the GPU export doubles what it is given (its density scale, the Cumulus' look), so it is given half
/// (`ScShape::gpu_layout`): both march one deck.
const SC_SIGMA: f32 = 0.1;
/// The recipe's id in the footprint grid's key, and the shape key's part.
const SC_RECIPE: f32 = 7.0;
const SC_KEY_PART: f32 = 17.0;
/// The footprint (cloud_sheets.rs's deck): the broad fbm's octaves (the 66 m grid holds four at Size 2.5; finer detail
/// is the 3D noise's), the cells (cycles a cell, their share over the broad field), Billows' lumps (cycles a cell, how
/// much a unit of Billows adds, the billowed octave's mean), how deep Rolls cut between their rows and how many rows a
/// cell (a Size apart: at the phase-1 deck's two Sizes and its 0.6 deep, the rows did not show in a deck), and the
/// column channel's frequency (cycles a cell).
const FIELD_OCTAVES: u32 = 4;
const CELL_FREQ: f32 = 2.5;
const CELL_SHARE: f32 = 0.55;
const LUMP_FREQ: f32 = 6.0;
const LUMP_AMOUNT: f32 = 0.5;
const BILLOW_MEAN: f32 = 0.38;
const ROLL_DEPTH: f32 = 1.0;
const ROLL_FREQ: f32 = 1.0;
const COLUMN_FREQ: f32 = 0.6;
/// The slab: the footprint noise over the threshold at which the deck stands its full height, the power its height rises
/// by (1: its top slopes down to the outline as a wedge; as the root, round shoulders, its patches stood as heaps from the
/// side), its top as a share of the shell from TOP_LOW where the column channel is empty to TOP_HIGH where it is full
/// (room above for turrets), how far a rim's underside lifts toward its top (a little: a deck's underside is flat, and at
/// 0.3 its cells hung as bubbles), and how far the underside lifts toward the deck's outline (EDGE_LIFT of the height as
/// (1 - the deck's depth)^2: a little, the base stays flat and the top meets it in a thin edge; at 0.4, a lens' rim met
/// half way up, the patches' undersides rounded up into hanging lumps; risen as the root of the footprint from a flat
/// underside, its wall stood vertical, and the footprint's light on it drew curtains on slant views).
const DOME_EDGE: f32 = 0.1;
const DOME_POWER: f32 = 0.5;
const TOP_LOW: f32 = 0.55;
const TOP_HIGH: f32 = 0.8;
const BOTTOM_LIFT: f32 = 0.05;
const EDGE_LIFT: f32 = 0.1;
/// The cells: Voronoi cells CELL_SIZE of Size apart (sites jittered CELL_JITTER of a cell, 175 m at the default Size:
/// aristea_wreck's overhead holds hundreds, round 3's 350 m cells merged into some ten groups a view) on the plane warped
/// by up to WARP of Size (WARP_FREQ cycles a cell of Size). A point stands in its cell's middle for its density as
/// F2 - F1 (in cells) rises to CELL_EDGE (its density fading to its rim's over a good part of the cell: no hard rim) and
/// for its height as it rises to HEIGHT_EDGE (a smoothstep: the cell a gentle dome; as the root of the way in, a crack's
/// wall stood vertical and drew curtains on slant views). The top dips toward a crack by TOP_DIP of the height (the
/// crack's column is that much thinner, its depth straight up some 11 against a middle's 25; lit through it, the GPU
/// takes a third of that dip, as each cell stood as a dark puff ringed bright) and the underside rises by UNDER_RISE
/// (as (1 - inside)^2: a bump under each cell, some 10 m), so seen from below or the side the deck reads as a layer
/// with a flat, gently bumpy underside. Each cell's height and density are its own, down to 1 - CELL_HEIGHTS and
/// 1 - CELL_DENSITIES; its rim and its own density shape the faces only, fading from FACE_M.0 to FACE_M.1 metres in.
const CELL_SIZE: f32 = 0.07;
const CELL_JITTER: f32 = 0.9;
const CELL_EDGE: f32 = 0.4;
const HEIGHT_EDGE: f32 = 0.5;
const TOP_DIP: f32 = 0.7;
const UNDER_RISE: f32 = 0.03;
const FACE_M: (f32, f32) = (15.0, 60.0);
/// On the GPU the cells' plane also wanders at their own scale (the shape tile, its broadest octave a cell long) by
/// WANDER of a cell, so their cracks bend, and their edges are ragged (F2 - F1 moved by up to RAGGED of a cell, the
/// detail tile RAG_FINER times finer than its own scale, read in 3D: read flat, it drew vertical streaks down a crack's
/// walls).
const WANDER: f32 = 0.6;
const RAGGED: f32 = 0.1;
const RAG_FINER: f32 = 8.0;
/// How much of the deck opens in soft holes: HOLE_MOST of it where the footprint covers HOLE_COVER.1 of the plane or
/// less, none from HOLE_COVER.0 (Cover's solve sets that share), HOLE_DEPTH.0 times that where the deck is shallow and
/// HOLE_DEPTH.1 times where it is deep (its noise DEEP over the threshold, from the first to the second): a broken deck
/// breaks into many small holes and patches, not into km-wide gaps, and the solve can always reach its Cover (half the
/// deck in holes at Cover 0.2 left it 0.03 of sky, not 0.2). The holes follow the footprint's share, not the Cover: a
/// point's density then rises with the share alone, so the solve's share rises with Cover (opened by the Cover itself,
/// they closed faster than Cover rose, and the solve's share fell from 0.79 at Cover 0.7 to 0.69 at 0.9).
/// The share maps to its quantile on the holes' noise (`holes`: two octaves of value noise HOLE_CELLS cells a node,
/// pulled down by HOLE_PULL along the cracks, so an open crack widens into a hole), HOLE_Q.0 + HOLE_Q.1 sqrt(share)
/// (fitted to that noise within 0.007 from 0.02 to 0.6, the edge's half way in), faded in over the first HOLE_FADE of
/// share. A hole's edge is graded over HOLE_SOFT of the noise (some 100-200 m) and its density thins to HOLE_FLOOR over
/// the band its faces close over (thin chips of deck by a hole stood as crisp, opaque flakes);
/// toward it the top comes down by HOLE_CLOSE.0 of the cell's full height and the underside rises by HOLE_CLOSE.1 (the
/// faces cross where the hole is open, so no skin is left there: meeting, they left a veil across every hole; its base
/// stays nearly flat: closing alike from both faces, the patches' undersides rounded up into hanging lumps), over
/// HOLE_THIN more of the noise: the deck thins toward its holes, so a broken deck's
/// patches are thin, flat-based lenses (full-height patches a few cells wide read as heaps from the side). As slits a
/// crack wide they read as sharp dark lines, and their straight walls hung as curtains. The share only falls as Cover
/// rises, so the density at a point never falls (design §3). The CPU deck opens the same holes, so Cover's solve, which
/// measures the CPU deck's opacity, holds the GPU's share of sky too.
const HOLE_MOST: f32 = 0.06;
const HOLE_COVER: (f32, f32) = (0.75, 0.35);
const HOLE_DEPTH: (f32, f32) = (1.3, 0.7);
const HOLE_CELLS: f32 = 3.5;
const HOLE_PULL: f32 = 0.05;
const HOLE_Q: (f32, f32) = (0.12, 0.56);
const HOLE_FADE: f32 = 0.03;
const HOLE_SOFT: f32 = 0.12;
const HOLE_CLOSE: (f32, f32) = (1.0, 0.2);
const HOLE_THIN: f32 = 0.1;
const HOLE_FLOOR: f32 = 0.3;
const DEEP: (f32, f32) = (0.15, 0.45);
const CELL_HEIGHTS: f32 = 0.15;
const CELL_DENSITIES: f32 = 0.4;
/// The species' forms from Billows: past TURRET_FROM a cell's chance of a turret grows to TURRET_SHARE at Billows 1
/// (castellanus: turrets on the cells), each rising TURRET_RISE of the shell over its cell as a dome (as tall as a
/// cell is wide and squared toward its middle, they stood as spikes); under
/// STRETCH_UNDER the cells are drawn out along the wind, up to 1 + STRETCH times at Billows 0 (lenticularis: smooth,
/// flat lens cells along the wind).
const TURRET_FROM: f32 = 0.6;
const TURRET_SHARE: f32 = 0.4;
const TURRET_RISE: f32 = 0.25;
const STRETCH_UNDER: f32 = 0.3;
const STRETCH: f32 = 1.5;
/// How deep a point stands into its cell on the cells' mean (the sun's grids and the light's cheap reads read so), for
/// its density and for its height.
const SUN_CELL_MEAN: (f32, f32) = (0.9, 0.8);
const WARP: f32 = 0.08;
const WARP_FREQ: f32 = 1.5;
/// The far band: from FAR_FROM to FAR_TO metres away the cells close and the relief and texture fade (GPU): from 8 km
/// the rows near the horizon stood as heaps.
const FAR_FROM: f32 = 4000.0;
const FAR_TO: f32 = 12000.0;
/// The fine mottling through the deck (GPU): its share of the density, the detail tile MOTTLE_FINER times finer than
/// its own scale (cells of some 145, 73 and 36 m at Size 2.5), so a deck seen from below is mottled, not smooth.
const MOTTLE: f32 = 0.35;
const MOTTLE_FINER: f32 = 10.0;
/// The balls' relief fades out on a pillow thinner than this, metres (a thin rim gets none: no loose puffs).
const RELIEF_FULL_M: f32 = 150.0;
/// The balls: the billow tile spans BALL_TILE cells of Size across (its largest balls a quarter of that), SQUASH times
/// less up; they move the top by BALL_TOP_M and the underside by BALL_BOTTOM_M a unit of the tile about BALL_MID, at
/// Billows 0.5 (1.5 times that at 1, half at 0).
const BALL_TILE: f32 = 1.6;
const SQUASH: f32 = 4.0;
const BALL_TOP_M: f32 = 15.0;
const BALL_BOTTOM_M: f32 = 15.0;
const BALL_MID: f32 = 0.5;
/// The skin over which the density ramps in, metres: SKIN_LEAST at Softness 0, SKIN_SOFT more at 1 (a soft gradation:
/// at 15-45 m the deck's faces drew hard rims; a quarter of the widest skin and the small lumps' reach stay under the
/// export march's 52 m margin, the reach past the slab and balls at which it looks for cloud).
const SKIN_LEAST: f32 = 40.0;
const SKIN_SOFT: f32 = 60.0;
/// Shading's thickness contrast: a crack's faces keep exp(-RIM_K x Shading) of a cell's middle density (0.6 at Shading
/// 0.5; the crack is thinner still through the top's dip, TOP_DIP).
const RIM_K: f32 = 1.0;
/// The sky through the gaps (the gap-lit dome): a point near an open gap (the deck thin over its threshold, or an
/// open crack) sees up to GAP_SKY of the dome past the cloud over it, GAP_BASE of that at the deck's underside, all of
/// it at its top (a gap's wall sees more sky higher up), fading over the deck's edge band and a crack's: a deck's rims
/// by its gaps are its brightest underside (stratocumuluswolken), and the slab share through the column takes the same
/// light (the sun diffused down through the deck beside the gap).
const GAP_SKY: f32 = 0.7;
const GAP_BASE: f32 = 0.5;
const GAP_EDGE: f32 = 0.15;
/// The GPU's texture: faint fibres along the wind on the faces (the detail tile FIBRE_FINER times finer than its own
/// scale across the wind, five times coarser along it, FIBRE of the density, within FACE_M of a face: a soft streaky
/// sheet; small lumps on the faces drew popcorn), the top skin's erosion by fine Worley noise, and the thin patches'
/// share.
const FIBRE_FINER: f32 = 30.0;
const FIBRE: f32 = 0.35;
const EROSION: f32 = 0.4;
const PATCH: f32 = 0.35;
/// The finer patches' share (the shape tile four times finer).
const PATCH_FINE: f32 = 0.2;
/// The deck's light on the export march (export/stratocumulus.rs: the template's knobs at a deck's values, and its own
/// march). A deck takes no heap diffusion and no open side (its light through the column is the slab share,
/// SLAB_DIFFUSE, and its own dome), its underside keeps the whole dome its column passes (that column is the cloud over it,
/// so the heap's growth with height would count it twice), and against the owner's real skies at their suns (103_2021,
/// 087_0826) a third of a heap's dome and 0.7 of its bounce: at the Cumulus' shares the deck stood the same grey all
/// round, 3.5 times 103_2021's dark side away from the sun. No low sun's dome boost: a deck's underside is lit through the
/// deck (its own column), not by the open sky beside a heap's base, and at the Cumulus' boost a low sun's deck stayed
/// white under a deep blue sky. The fine step's least is 8 m (the Cumulus' 2 m sampled round 2's soft skin some 300 times
/// a ray). The section holds these as its knob values; the slab share and its lobe are uploaded (`sc_r`).
///
/// The slab diffuse share (`sc_r.x`: sun.y / pi times this, through the column over the point: the CPU's slab family
/// through the column, `RayLight::diffuse_through_column`; the export multiplies its sun's sums by 3, `xf.x`, so a third
/// is the slab law's own share: at a half the overhead deck under a 14 degree sun stood 10 times the zenith sky's
/// luminance; 0.13 with the deck's depth at 0.1 /m and the lobe below: the overhead deck at 103_2021's sun stands 1.1
/// times the clear sky beside it, the real one 1.6).
const SLAB_DIFFUSE: f32 = 0.13;
/// The slab share's part along the sun's lobe (`sc_r.y`: the second octave's dual lobe, a mean of 1, so the deck's light
/// keeps its sum): a deck under a low sun is bright toward it and dark away (103_2021: 4.8 times from toward to away
/// against the sun's irradiance, 087_0826 3.2), where the slab share alone drew it the same all round (1.2).
const SLAB_LOBE: f32 = 1.5;
/// The CPU march's lattice in air is never coarser than this, metres (the Cumulus' 40 m): a deck is a wide sheet, and
/// its fallback march is rough.
const LATTICE_M: f32 = 80.0;

/// The deck's footprint recipe: its rows that change the noise.
#[derive(Clone, Copy, Debug)]
pub(super) struct ScRecipe {
    billows: f32,
    rolls: f32,
}

impl ScRecipe {
    /// The footprint's noise, its column channel, the cells' warp (cells along and across), and its cells' own noise at
    /// `f` (wind frame, cells) with the streams `s`: cloud_sheets.rs's deck recipe at FIELD_OCTAVES.
    fn at(&self, f: Vec2f, s: &Streams) -> Node {
        let field = fbm2(f.x, f.y, FIELD_OCTAVES, s.0[0]);
        let cells = billow2(f.x * CELL_FREQ + 1.7, f.y * CELL_FREQ + 4.1, 1, s.0[3]);
        let mut n = CELL_SHARE * cells + (1.0 - CELL_SHARE) * field;
        if self.billows > 0.0 {
            n += self.billows * LUMP_AMOUNT * (billow2(f.x * LUMP_FREQ + 3.1, f.y * LUMP_FREQ + 7.7, 2, s.0[1]) - BILLOW_MEAN);
        }
        if self.rolls > 0.0 {
            let warp = fbm2(f.x * 0.25, f.y * 0.25, 1, s.0[2]) - 0.5;
            n *= 1.0 - self.rolls * ROLL_DEPTH * (1.0 - wave(f.y * ROLL_FREQ + 0.35 * warp));
        }
        let column = fbm2(f.x * COLUMN_FREQ + 5.3, f.y * COLUMN_FREQ + 2.1, 2, s.0[4]);
        let wx = fbm2(f.x * WARP_FREQ + 3.1, f.y * WARP_FREQ + 7.4, 2, s.0[5]) - 0.5;
        let wz = fbm2(f.x * WARP_FREQ + 9.2, f.y * WARP_FREQ + 1.3, 2, s.0[6]) - 0.5;
        [n.clamp(0.0, 1.0), column, 2.0 * WARP * wx, 2.0 * WARP * wz, cells]
    }

    /// `at` at a ground point (metres east and south) in the frame `frame` of cells `cell_m` wide: the warp turned back
    /// onto the plane, in metres (what the footprint grid holds).
    fn at_m(&self, frame: &WindFrame, s: &Streams, cell_m: f32, x: f32, z: f32) -> Node {
        let [n, column, along, across, cells] = self.at(frame.of(vec2f(x / cell_m, z / cell_m)), s);
        let warp = frame.to_plane(vec2f(along, across)) * cell_m;
        [n, column, warp.x, warp.y, cells]
    }

    /// The fields its quantile tables are measured for (design §3.1's fingerprint).
    fn fields(&self) -> [f32; 2] {
        [self.billows, self.rolls]
    }
}

/// What a Stratocumulus volume adds to its `VolumeShape`: its recipe and the numbers its density reads.
pub(super) struct ScShape {
    recipe: ScRecipe,
    /// The billow tile's scale (1 / metres across) for the balls, and the cells' size (metres) and hash seed.
    ball_scale: f32,
    cell_m: f32,
    cell_seed: u32,
    ball_top_m: f32,
    ball_bottom_m: f32,
    /// The density at a rim, a share of a cell's middle (exp(-RIM_K x Shading)).
    rim: f32,
    skin_m: f32,
    /// The wind's direction on the plane, how far the cells are drawn out along it (1 none), and a cell's chance of a
    /// turret (Billows' forms: TURRET_*, STRETCH*).
    downwind: Vec2f,
    stretch: f32,
    turrets: f32,
    /// The holes' noise scale (1 / metres).
    hole_k: f32,
}

/// The volume of a Stratocumulus `spec` (design §2.2), None at no cover or no density: its footprint grid looked up or
/// built (rows spread by `run`, `cancelled` polled), the seed's tiles, and Cover's share solved as the Cumulus' is
/// (design §3).
pub(super) fn build(spec: &VolumeSpec, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<Option<VolumeShape>, Cancelled> {
    let b = spec.layer.stratocumulus_params();
    let c = spec.cover;
    if !(c > 0.0) || !(b.density > 0.0) {
        return Ok(None);
    }
    let cell_m = b.size_km * 1000.0;
    let stream = cloud_layer_seed(spec.map_seed, spec.layer.seed, spec.genus);
    let streams = Streams::of(stream);
    let drift_cells = spec.layer.drift_km.unwrap_or(0.0) * 1000.0 / cell_m;
    let frame = WindFrame::new(spec.downwind, drift_cells);
    let recipe = ScRecipe { billows: b.billows, rolls: b.rolls };
    let (base_m, top_m) = (b.base_m, b.tops_m.max(b.base_m + 1.0));
    let thickness = top_m - base_m;
    // The deck stands anywhere in its shell where it has footprint; it has no lean.
    let bounds = move |_: [f32; 2], _: [f32; 2]| (base_m, top_m);
    let margin = SkipMargin { across_m: 1.0, below_m: 0.0, above_m: 0.0 };
    let grid_key = footprint_grid_key(&[SC_RECIPE, b.size_km, b.billows, b.rolls, spec.downwind.x, spec.downwind.y, drift_cells, base_m, top_m], stream, GRID_SIZE, GRID_SPAN_M);
    let at_m = |x: f32, z: f32| recipe.at_m(&frame, &streams, cell_m, x, z);
    let grid = shared_footprint_grid(grid_key, &|| FootprintGrid::build(GRID_SIZE, GRID_SPAN_M, &at_m, &bounds, margin, cancelled, run))?;
    let noise = noise_for(spec.map_seed, cancelled, run)?;
    let billow = noise.billow(cancelled, run)?;
    let place = |k: u32| (hash_u32(stream ^ k.wrapping_mul(0x68e3_1da4)) >> 8) as f32 * (1.0 / 16_777_216.0);
    let size_km = b.size_km;
    let sigma = SC_SIGMA * b.density;
    let steps = spec.march_steps.clamp(STEPS_LEAST, STEPS_MOST);
    let ball_scale = 1.0 / (BALL_TILE * cell_m);
    let billows = 0.5 + b.billows.clamp(0.0, 1.0);
    let sc = ScShape {
        recipe,
        ball_scale,
        cell_m: CELL_SIZE * cell_m,
        cell_seed: hash_u32(stream ^ 0x3c6e_f372),
        ball_top_m: BALL_TOP_M * billows,
        ball_bottom_m: BALL_BOTTOM_M * billows,
        rim: (-RIM_K * b.shading.clamp(0.0, 1.0)).exp(),
        skin_m: SKIN_LEAST + SKIN_SOFT * b.softness.clamp(0.0, 1.0),
        downwind: spec.downwind,
        stretch: 1.0 + STRETCH * ((STRETCH_UNDER - b.billows) / STRETCH_UNDER).clamp(0.0, 1.0),
        turrets: TURRET_SHARE * ((b.billows - TURRET_FROM) / (1.0 - TURRET_FROM)).clamp(0.0, 1.0),
        hole_k: 1.0 / (HOLE_CELLS * CELL_SIZE * cell_m),
    };
    let bits = f32::from_bits;
    let key = footprint_grid_key(&[b.softness, b.shading, b.billows, b.density, sigma, bits(spec.map_seed), bits(grid_key as u32), bits((grid_key >> 32) as u32)], stream, TOP_COLS, SC_KEY_PART);
    let set = quantile_set(fingerprint(spec.genus, FOOTPRINT_PART, &recipe.fields()));
    let mut shape = VolumeShape {
        base_m,
        top_m,
        thickness,
        cell_m,
        frame,
        recipe: VolumeRecipe::Cumulus(CumulusRecipe { clustering: 0.0, streets: 0.0 }),
        streams,
        grid,
        grid_key,
        remap: CoverRemap::new(set.clone(), c, FOOTPRINT_EDGE, stream),
        threshold: 0.0,
        gated: false,
        share: c,
        k: 1.0,
        // The Cumulus' tables: none read for a deck (the GPU upload sends them at their lengths).
        tops: vec![0.0; TOP_COLS],
        rise: vec![0.0; RISE_ROWS],
        rise_threshold: 0.0,
        levels: vec![0.0; LEVEL_ROWS + 1],
        shape_scale: ball_scale,
        detail_scale: 1.0 / (DETAIL_TILE_M * size_km),
        detail_texel_m: DETAIL_TILE_M * size_km / noise.detail.size() as f32,
        detail_mean: noise.detail.mean(),
        noise,
        billow,
        offset: vec3f(place(1), place(2), place(3)),
        sigma,
        carve: 1.0,
        billow_cut: BALL_MID,
        span: 0.3,
        erosion: 0.0,
        lean: Vec2f::default(),
        steps,
        lattice_m: LATTICE_M,
        cover: c,
        stream,
        key,
        genus: spec.genus,
        stratus: None,
        sc: Some(sc),
        ns: None,
    };
    let measured = Streams::of(MEASURE_SEED);
    let n = |_: u32, x: f32, y: f32| recipe.at(vec2f(x, y), &measured)[0];
    shape.solve_share(c, &set, &n);
    shape.key = footprint_grid_key(&[bits(key as u32), bits((key >> 32) as u32), shape.threshold, shape.share, shape.gated as u32 as f32], stream, TOP_COLS, SC_KEY_PART);
    Ok(Some(shape))
}

impl ScShape {
    /// The recipe at a ground point (metres east and south): what the footprint grid holds.
    pub(super) fn recipe_m(&self, shape: &VolumeShape, x_m: f32, z_m: f32) -> Node {
        self.recipe.at_m(&shape.frame, &shape.streams, shape.cell_m, x_m, z_m)
    }

    /// The plain cells at the plane point `w` (metres; the grid's warped plane): F2 - F1 in cells (0 on a crack), and the
    /// cell's own height share (past 1 by TURRET_RISE on a turret) and density share. The cells lie in the wind's frame,
    /// drawn out along it by `stretch`. cloud_gpu's `sccell` computes the same cells on a plane that also wanders, with
    /// ragged edges (the same hash, `hu` there).
    fn cells(&self, w0: Vec2f) -> (f32, f32, f32) {
        let (dw, k) = (self.downwind, self.stretch);
        let w = vec2f((w0.x * dw.x + w0.y * dw.y) / k, w0.y * dw.x - w0.x * dw.y);
        let g = w / self.cell_m;
        let (ix, iy) = (g.x.floor(), g.y.floor());
        let (mut f1, mut f2, mut id) = (9.0f32, 9.0f32, 0u32);
        for j in 0..3 {
            for i in 0..3 {
                let (cx, cy) = (ix + i as f32 - 1.0, iy + j as f32 - 1.0);
                let h = hash_u32(hash_u32(self.cell_seed ^ (cx as i32) as u32) ^ ((cy as i32) as u32).wrapping_mul(2_654_435_769));
                let dx = g.x - (cx + 0.5 + CELL_JITTER * ((h & 0xffff) as f32 / 65535.0 - 0.5));
                let dy = g.y - (cy + 0.5 + CELL_JITTER * ((h >> 16) as f32 / 65535.0 - 0.5));
                let d = dx * dx + dy * dy;
                if d < f1 {
                    (f2, f1, id) = (f1, d, h);
                } else if d < f2 {
                    f2 = d;
                }
            }
        }
        let h = hash_u32(id ^ 1_013_904_223);
        let turret = (hash_u32(id ^ 2_246_822_519) & 0xffff) as f32 / 65535.0 < self.turrets;
        let height = if turret { 1.0 + TURRET_RISE } else { 1.0 - CELL_HEIGHTS * ((h & 0xffff) as f32 / 65535.0) };
        (f2.sqrt() - f1.sqrt(), height, 1.0 - CELL_DENSITIES * ((h >> 16) as f32 / 65535.0))
    }

    /// The holes' noise at the plane point `w` (metres): two octaves of value noise on the cells' hash, 0..1.
    /// cloud_gpu's `schole` computes the same.
    fn holes(&self, w: Vec2f) -> f32 {
        let seed = self.cell_seed ^ 3_141_592_653;
        let (mut sum, mut amp, mut k) = (0.0f32, 1.0f32, self.hole_k);
        for o in 0..2u32 {
            let g = vec2f(w.x * k + 0.37 * o as f32, w.y * k + 0.61 * o as f32);
            let (ix, iy) = (g.x.floor(), g.y.floor());
            let (fx, fy) = (g.x - ix, g.y - iy);
            let (ux, uy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
            let so = seed ^ o.wrapping_mul(2_654_435_761);
            let at = |x: f32, y: f32| (hash_u32(hash_u32(so ^ (x as i32) as u32) ^ ((y as i32) as u32).wrapping_mul(2_654_435_769)) & 0xffff) as f32 / 65535.0;
            let (a, b, c, d) = (at(ix, iy), at(ix + 1.0, iy), at(ix, iy + 1.0), at(ix + 1.0, iy + 1.0));
            let low = a + (b - a) * ux;
            sum += amp * (low + ((c + (d - c) * ux) - low) * uy);
            amp *= 0.5;
            k *= 2.0;
        }
        sum / 1.5
    }

    /// The share of the deck open in holes where the footprint covers `plane` of the plane (HOLE_MOST by HOLE_COVER).
    fn hole_share(plane: f32) -> f32 {
        HOLE_MOST * ((HOLE_COVER.0 - plane) / (HOLE_COVER.0 - HOLE_COVER.1)).clamp(0.0, 1.0)
    }

    /// The share of the deck open in holes at the deck's noise `over` its threshold (by the footprint's share of the
    /// plane, more where the deck is shallow) and its quantile on the holes' noise: cloud_gpu's `scdens` computes the same.
    fn hole_threshold(&self, shape: &VolumeShape, over: f32) -> (f32, f32) {
        let depth = smoothstep(DEEP.0, DEEP.1, over);
        let share = (Self::hole_share(shape.share) * (HOLE_DEPTH.0 + (HOLE_DEPTH.1 - HOLE_DEPTH.0) * depth)).clamp(0.0, 0.75);
        (share, HOLE_Q.0 + HOLE_Q.1 * share.sqrt())
    }

    /// The deck at `p` (`h01` through the shell) on the footprint node `node`, in a cell `inside` deep for its height
    /// (HEIGHT_EDGE's smoothstep) with its own `height` share (past 1 a turret's rise): the metres into it (up from its
    /// underside or down from its top, the nearer), how far the point stands into the deck and its cell together (0 on
    /// an outline or a crack, 1 in a cell's middle) and the metres down from its top; toward a hole (`thin`, 0 to 1) its
    /// top comes down by HOLE_CLOSE.0 of the full height and its underside rises by HOLE_CLOSE.1. cloud_gpu's `scdens`
    /// computes the same, with the balls lumping both faces.
    fn em0(&self, shape: &VolumeShape, h01: f32, node: &Node, inside: f32, height: f32, thin: f32) -> (f32, f32, f32) {
        let [n, column, ..] = *node;
        let deck = ((n - shape.threshold) / DOME_EDGE).clamp(0.0, 1.0);
        let a = deck * inside;
        let (mh, turret) = (1.0 - 0.5 * CELL_HEIGHTS, (height - 1.0).max(0.0));
        let height = mh + (height.min(1.0) - mh) * inside;
        let dip = 1.0 - inside;
        let full = (TOP_LOW + (TOP_HIGH - TOP_LOW) * column.clamp(0.0, 1.0)) * height;
        let top = (full * deck.powf(DOME_POWER) * (1.0 - TOP_DIP * dip) + turret * inside * deck).min(1.0) - HOLE_CLOSE.0 * thin * full;
        // The underside is a share of the cell's full height, not of its top, so it only falls as Cover rises: the
        // density at a point never falls as Cover rises (design §3). As a share of the top, it rose past points.
        let bottom = full * (BOTTOM_LIFT * (1.0 - a) + EDGE_LIFT * (1.0 - deck) * (1.0 - deck) + UNDER_RISE * dip * dip + HOLE_CLOSE.1 * thin);
        let to_top = (top - h01) * shape.thickness;
        let to_bottom = (h01 - bottom) * shape.thickness;
        (to_top.min(to_bottom), a, to_top)
    }

    /// The sky its gaps show a point `h01` through the deck on the footprint noise `n`, `near` a hole (0 to 1): the share
    /// of the dome past the cloud over it that reaches it through the gaps nearby (the gap-lit dome): GAP_SKY where the
    /// deck's edge band (GAP_EDGE of noise over the threshold) or a hole begins, GAP_BASE of that at the underside.
    /// cloud_gpu's `scdens` computes the same.
    fn sky(&self, shape: &VolumeShape, h01: f32, n: f32, near: f32) -> f32 {
        let deck = 1.0 - ((n - shape.threshold) / GAP_EDGE).clamp(0.0, 1.0);
        GAP_SKY * (deck * deck).max(near) * (GAP_BASE + (1.0 - GAP_BASE) * h01.clamp(0.0, 1.0))
    }

    /// The depth over a point its march's dome reads (`cloud_volume_light::step`'s tau_up) for the depth `tau_up` of the
    /// cloud over it and the sky its gaps show (`sky`): the dome T_d(tau_up) gains `sky` of what the cloud over it takes.
    fn lit_up(tau_up: f32, sky: f32) -> f32 {
        let above = crate::cloud_light::diffuse_transmittance(tau_up);
        let lit = above + (1.0 - above) * sky.clamp(0.0, 1.0);
        (1.0 / lit - 1.0) / crate::cloud_light::TD_K
    }

    /// The footprint after the cover remap at the ground point `q` on the noise `n` (over the threshold).
    fn cover(&self, shape: &VolumeShape, q: Vec2f, n: f32) -> f32 {
        if shape.gated {
            shape.remap.density(n, shape.threshold, shape.frame.of(q / shape.cell_m))
        } else {
            cover_density(n, shape.threshold, FOOTPRINT_EDGE)
        }
    }

    /// `VolumeShape::cheap` for a deck: the pillows on plain cells (no wander, raggedness or balls) with a soft skin, no
    /// detail (its skin weight 0); its envelope is where its density stands. The sun's field is the view's.
    pub(super) fn cheap(&self, shape: &VolumeShape, p: Vec3f, footprint_m: f32) -> Cheap {
        let h01 = (p.y - shape.base_m) / shape.thickness;
        let q = vec2f(p.x, p.z);
        if !(h01 > 0.0 && h01 < 1.0) {
            return Cheap { h01, q, ..Cheap::default() };
        }
        if !(p.x.abs() < shape.grid.half_m() && p.z.abs() < shape.grid.half_m()) {
            return Cheap { h01, q, ..Cheap::default() };
        }
        let node = shape.grid.sample(q.x, q.y, footprint_m);
        let n = node[0];
        if !(n > shape.threshold) {
            return Cheap { h01, q, ..Cheap::default() };
        }
        let f = self.cover(shape, q, n);
        let air = Cheap { h01, q, f, ..Cheap::default() };
        if !(f > 0.0) {
            return air;
        }
        let w = vec2f(p.x + node[2], p.z + node[3]);
        let (e, height, density) = self.cells(w);
        // How far into its cell for its density (its rim) and for its height (its bump); each cell's own density blended
        // to the mean toward its edge (its height in em0), the cells shaping the faces only, and the soft holes.
        let (cell, cell_h) = (smoothstep(0.0, CELL_EDGE, e), smoothstep(0.0, HEIGHT_EDGE, e));
        let md = 1.0 - 0.5 * CELL_DENSITIES;
        let (share, open_t) = self.hole_threshold(shape, n - shape.threshold);
        let hf = self.holes(w) - HOLE_PULL * (1.0 - cell_h);
        let fade = (share / HOLE_FADE).min(1.0);
        let thin = (1.0 - smoothstep(open_t - HOLE_SOFT, open_t + HOLE_SOFT + HOLE_THIN, hf)) * fade;
        let (em, _, to_top) = self.em0(shape, h01, &node, cell_h, height, thin);
        // The envelope is where density stands (a quarter skin past the pillows): wider, a ray beside a deck it never met
        // went into cloud mode, and the samples of the deck it met next moved with Cover (its march's opacity fell 2e-7
        // as Cover rose).
        if !(em > -0.25 * self.skin_m) {
            return air;
        }
        let cw = 1.0 - smoothstep(FACE_M.0, FACE_M.1, em);
        // A cell's own density only lowers it near a face, so a point the slab grows past never thins as Cover rises.
        let rim = 1.0 - (1.0 - self.rim) * cw;
        let crack = rim + (1.0 - rim) * cell;
        let gate = (3.0 * f).min(1.0) * crack * (1.0 - (1.0 - (md + (density - md) * cell)) * cw) * (1.0 - (1.0 - HOLE_FLOOR) * thin);
        let view = (em / self.skin_m + 0.25).clamp(0.0, 1.0) * gate;
        // Its own dome (the gap-lit dome): T_d of its own column over the point (its density under the top's skin: the
        // skin ramps in over three quarters of it, so the column misses 0.375 of a skin), gaining the sky its gaps show.
        // (the column takes the plain slab's gate: the cells' rims and own densities stand at the faces only; a crack is
        // thinner through the top's dip)
        let near = (1.0 - smoothstep(open_t - 0.3, open_t + 0.3, hf)) * fade;
        let plain = (3.0 * f).min(1.0) * (1.0 - (1.0 - HOLE_FLOOR) * thin);
        let up = Self::lit_up(shape.sigma * plain * (to_top - 0.375 * self.skin_m).max(0.0), self.sky(shape, h01, n, near));
        Cheap { view, h01, q, f, skin: 0.0, sun: view, envelope: true, up: Some(up) }
    }

    /// The sun's field its grids and live sun steps read (`VolumeShape::extinction` over sigma): the pillows at their
    /// cells' mean (SUN_CELL_MEAN into a cell, a mean cell's height and density, a crack half closed), no cells. The sun
    /// grid's 47 m cells are coarser than a crack, and the cells' lookup is the dearest part of a density read: through
    /// it a 1024 preview's grids took 2.7 s of its 5.2 s (round 1, contended), at the Cumulus' 0.1 s without.
    pub(super) fn sun_field(&self, shape: &VolumeShape, p: Vec3f) -> f32 {
        let h01 = (p.y - shape.base_m) / shape.thickness;
        if !(h01 > 0.0 && h01 < 1.0) || !(p.x.abs() < shape.grid.half_m() && p.z.abs() < shape.grid.half_m()) {
            return 0.0;
        }
        let node = shape.grid.sample(p.x, p.z, 0.0);
        let n = node[0];
        if !(n > shape.threshold) {
            return 0.0;
        }
        let f = self.cover(shape, vec2f(p.x, p.z), n);
        let (em, ..) = self.em0(shape, h01, &node, SUN_CELL_MEAN.1, 1.0 - 0.5 * CELL_HEIGHTS, 0.0);
        let gate = (3.0 * f).min(1.0) * (self.rim + (1.0 - self.rim) * SUN_CELL_MEAN.0.sqrt()) * (1.0 - 0.5 * CELL_DENSITIES);
        (em / self.skin_m + 0.25).clamp(0.0, 1.0) * gate
    }

    /// The deck's numbers as the GPU export's `scdens` reads them (design §12.3), and its shares of the light law.
    pub(super) fn gpu_layout(&self, shape: &VolumeShape, sun_up: f32, out: &mut UploadBuilder) {
        // the export doubles the shell's extinction (xa.x, the Cumulus' density scale): it is given half the deck's
        out.uniform("shell", [shape.base_m, shape.top_m, shape.thickness, 0.5 * shape.sigma]);
        out.uniform("sc_a", [DOME_EDGE, DOME_POWER, TOP_LOW, BOTTOM_LIFT]);
        out.uniform("sc_b", [self.ball_scale, SQUASH, self.ball_top_m, self.ball_bottom_m]);
        out.uniform("sc_c", [BALL_MID, self.rim, self.skin_m, 0.0]);
        out.uniform("sc_d", [0.0, EROSION, PATCH, 0.0]);
        out.uniform("sc_e", [self.cell_m, CELL_EDGE, 0.0, CELL_JITTER]);
        out.uniform("sc_f", [PATCH_FINE, RELIEF_FULL_M, CELL_HEIGHTS, CELL_DENSITIES]);
        out.uniform("sc_g", [(self.cell_seed & 0xffff) as f32, (self.cell_seed >> 16) as f32, WANDER, UNDER_RISE]);
        out.uniform("sc_h", [DEEP.0, DEEP.1, RAGGED, shape.detail_scale * RAG_FINER]);
        out.uniform("sc_i", [FAR_FROM, FAR_TO, MOTTLE, shape.detail_scale * MOTTLE_FINER]);
        out.uniform("sc_j", [TOP_DIP, GAP_SKY, GAP_BASE, GAP_EDGE]);
        out.uniform("sc_k", [HOLE_DEPTH.0, HOLE_DEPTH.1, HOLE_THIN, Self::hole_share(shape.share)]);
        out.uniform("sc_l", [HEIGHT_EDGE, SUN_CELL_MEAN.0, SUN_CELL_MEAN.1, EDGE_LIFT]);
        out.uniform("sc_m", [self.downwind.x, self.downwind.y, self.stretch, self.turrets]);
        out.uniform("sc_n", [TURRET_RISE, TOP_HIGH, HOLE_CLOSE.1, 0.0]);
        out.uniform("sc_o", [self.hole_k, HOLE_PULL, HOLE_SOFT, HOLE_CLOSE.0]);
        out.uniform("sc_p", [HOLE_Q.0, HOLE_Q.1, HOLE_FADE, HOLE_FLOOR]);
        out.uniform("sc_q", [shape.detail_scale * FIBRE_FINER, FIBRE, FACE_M.0, FACE_M.1]);
        out.uniform("sc_r", [SLAB_DIFFUSE * sun_up.max(0.0) / std::f32::consts::PI, SLAB_LOBE, 0.0, 0.0]);
    }
}
