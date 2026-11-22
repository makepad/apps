//! The cloud volume renderer (design §2 and §12), proved on Cumulus first.
//!
//! - **Shape** ([`VolumeShape`]). A layer is a shell between its base and its top, in its wind frame with its drift.
//!   Its density at a point (design §2.2, §12.1 item 4, §12.7 item 5) is read cheap first: the footprint grid (where
//!   cloud stands, its column channel, its domain warp and its cloud's strength), then the column's top from a small
//!   table, then the 3D billow tile on the warped domain at the cloud's size level (§12.9 item 1), which carves a skin:
//!   how deep a point lies inside the cloud's envelope (across from its outline, down from its top) sets how much of
//!   the noise it keeps, so the rims and tops are balls and the core is solid. The detail erosion comes last, only on that skin and only while a detail texel is larger than
//!   the texel's footprint on the ray. The cheap value bounds the full one (detail only erodes).
//! - **Cover** (design §3). The footprint's threshold comes from its noise's quantile table, at a share c / k, so
//!   the share of sky the volume hides follows Cover though its edges are eroded and thin and its sides stand up: k
//!   is what the upper sky's views see hidden per unit of footprint share, measured at build on COVER_VIEWS views
//!   above COVER_FROM_DEG from points round the viewer, and solved by a bracketed secant in at most SOLVE_PASSES views
//!   of them. Vertical columns alone (design §3's
//!   first form) left the sides out: a Cumulus seen 20 to 90 degrees up hides far more than its columns do (Cover 0.2
//!   hid 0.46 of that sky).
//! - **The march** ([`CloudVolume::march`], design §12.1 items 2-4, 6-9, §12.6 item 4). In air it steps on a lattice
//!   of the step budget (never coarser than LATTICE_MOST_M), and crosses an empty cell of the footprint's max pyramid
//!   (no footprint, or the ray over its tops or under its bases) in one move to the next lattice point past it; on
//!   finding cloud it goes back one step, finds the edge by halving, and walks a mean free path at full density a step
//!   (from SKIN_STEP of one at the sunlit skin, never longer than the lattice step), and returns to the lattice after
//!   EMPTY_TO_AIR empty samples; where a run of cloud ends its exit is found by halving too, and its last step ends
//!   there at no extinction. The start is offset by a blue-noise tile per texel. Every lit step is the light law's
//!   energy-conserving step (`cloud_volume_light::step`) from its sample to the next: the extinction through it the
//!   mean of the two where they are one run, the sun's depth linear between them (the forward difference; a run's
//!   last sample takes the backward one), from the sun-depth grid's hybrid until the ray is 90 % opaque and its grid
//!   alone past that, the dome's depth from tau_up, and the powder's from the first two live sun steps. A flat entry's
//!   height cuts the march into segments, one record each. Held is the same march at the held width, on the held grid
//!   levels; its density is the released one's (LAW_WIDTH_LEAST), only its jitter is its own.
//! - The skip is exact: the lattice points it jumps over would each read no cloud, so a march with it and one without
//!   it see the same samples ([`VolumeShape::opacity`]'s `skip`).
//!
//! Positions are metres round the viewer: x east, y the height above the ground under the point, z south. The view
//! ray's height follows the curved Earth (`volumetric::height_at`) and the sun's rays are straight in this frame, so
//! the density, the march and the sun's grids all share one frame (a curved shell would bend a 32 km sun ray by about
//! 80 m against the grid).

use makepad_draw::*;
use makepad_render_material::ibl;
use crate::caster::RowRunner;
use crate::cloud_cover::{cover_density, fingerprint, quantile_set, CoverRemap, MEASURE_SEED};
use crate::cloud_light::{CoverSample, LightFamily};
use crate::cloud_sheets::{wave, Streams, WindFrame};
use crate::cloud_volume::{blue_jitter, footprint_grid_key, shared_by_key, shared_footprint_grid, FootprintGrid, GridCache, Node, SkipMargin};
use crate::cloud_volume_light::{step, RayLight, VolumeMarch, POWDER_REACH_M};
use crate::cloud_volume_sun::{SunDepthGrid, SunGridSpec, TauUpGrid, COARSE_RADIUS_M, SUN_CELL_M};
use crate::noise::{fbm2, hash_u32};
use crate::volumetric::{height_at, noise_for, shell_distance, Cancelled, NoiseVolume, Tile3, DETAIL_TILE_M, MIN_TRANSMITTANCE, RANGE_M, SHAPE_TILE_M};
use crate::{arr, cloud_layer_seed, vec, CloudGenus, CloudLayerParams};
use crate::cloud_volume_nimbostratus::{self as nimbostratus, NsShape};
use std::sync::{Arc, Mutex, OnceLock};

/// The Stratus' own recipe, shell and density on this machinery (design §2.2's Stratus row).
#[path = "cloud_volume_stratus.rs"]
mod stratus;
pub(crate) use stratus::horizon_fade as stratus_horizon_fade;
/// The Stratocumulus' deck (design §2.2's table): its own footprint and density on this renderer.
#[path = "cloud_volume_stratocumulus.rs"]
mod stratocumulus;

/// A Cumulus' extinction per metre at Density 1 (design §2.2's table): a mean free path of about 17 m.
pub const CUMULUS_SIGMA: f32 = 0.06;
/// The footprint grid: nodes along an edge, and its span round the viewer, the march's range and a margin for the lean
/// and the live sun steps (66 m cells).
pub const GRID_SIZE: usize = 1024;
pub const GRID_SPAN_M: f32 = 2.0 * (RANGE_M + 2000.0);
/// The footprint recipe (design §2.4, §12.9 item 3): one cloud per cell of Size, its reach at its base CELL_REACH cells
/// times its own share from REACH_LEAST to all, its strength from STRENGTH_LEAST to all, and SATELLITES weaker clouds
/// of the same reach over the cell's middle SATELLITE_JITTER, their strengths SATELLITE_STRENGTH: a cloud is as wide
/// as its bump stands over the threshold, so a weak one is a small, low cloud (cumulus humilis) and the sky holds few
/// big, many small.
/// Each is stretched along the wind by up to STRETCH and squeezed across by as much. The clouds meet near their
/// largest, as the BLEND-norm of their bumps: joined as 1 - prod(1 - b), two fringes each under the threshold added up
/// over it, and 0.52 of the footprint lay in bodies of seven clouds; the plain largest's crease where two clouds meet
/// was read off the grid by 0.109 at Size 0.3, over the 0.1 held there. Its domain is warped once by WARP cells (cycles
/// WARP_FREQ a cell), its column channel a smooth noise COLUMN_FREQ cycles a cell. The grid's 66 m cells hold every
/// octave of it from Size 0.75 up (within 0.02, 0.004 at 1.5): its finest wavelengths are some 400 m at 1.5 (design
/// §12.1 item 1; finer detail is the 3D noise's), which is why the small clouds are weak bumps, not small ones (bumps
/// a third as wide were read off by 0.036 at Size 1.5). They shrink with Size and the cells do not, so a smaller Size
/// is held less closely (0.095 at 0.3).
const CELL_REACH: f32 = 0.62;
const REACH_LEAST: f32 = 0.75;
const STRENGTH_LEAST: f32 = 0.5;
const SATELLITES: u32 = 3;
const SATELLITE_STRENGTH: (f32, f32) = (0.25, 0.55);
/// The satellites stand at least this far at random in their cell (a share of it round its middle), Clustering's
/// jitter where that is more: the default Clustering's 0.7. Anywhere in the cell (1) at the default, the sunlit sides
/// (`the_sunlit_side_is_brighter`) fell to 1.287 times the shaded ones, under the 1.3 held: more of the weak clouds
/// stood clear of the big ones, thin enough to be lit through from either side.
const SATELLITE_JITTER: f32 = 0.7;
const BLEND: f32 = 2.5;
const STRETCH: f32 = 2.0;
/// The fifth grid channel is the strength of the cloud a point stands in (design §12.9 items 1 and 3), weighed between
/// clouds as their bumps' BLEND-th powers are and toward a full one by STRENGTH_CLEAR where none stands: with the
/// threshold it gives how wide the cloud stands, which its balls follow. The ratio is kept per node and the mips average
/// it. Carried as its two sums (powers times strengths, and powers) and divided after the read, a weak cloud 12-30 km
/// out (setup 1's seed) read at a far texel's 8 cells was pulled toward the strong clouds round it (their powers
/// outweigh it): 0.523 against its own 0.484, 14 % of them off by over 0.1 (the ratio: 0.493, 3 %), and the light
/// gate's renderer p99 rose 4-52 % a sun. The clear nodes' full strength reaches no weak cloud there (none read over
/// 0.9 either way); only at Size 0.3's rims did the sums read closer (a mean 0.006 off at level 0 against 0.011).
const STRENGTH_CLEAR: f32 = 1.0e-6;
const WARP: f32 = 0.1;
const WARP_FREQ: f32 = 0.5;
const COLUMN_REACH: f32 = 1.4;
/// Clustering's broad mask (cycles a cell) and how deep Streets cut between their rows (as the sheet's).
const CLUSTER_FREQ: f32 = 0.2;
const STREET_DEPTH: f32 = 0.7;
/// The footprint's cover edge (the remap's width over its noise): narrow, so a small cloud at a low Cover is as solid
/// as a large one (cumulus humilis), only smaller.
const FOOTPRINT_EDGE: f32 = 0.15;
/// A cloud's height rises from its footprint's edge to its column's top where its noise is DOME_EDGE over the
/// threshold, as the way there to the power DOME_POWER (design §2.4's radius-by-height curve, §12.7 item 5): its
/// sides rise from a flat base and round over into its top, and a cloud that barely passes the threshold (the first
/// ones as Cover rises) stays low. The root (power 0.5) stood the lower half of every cloud up as a wall within 60-130 m
/// of its outline, the 2D outline extruded, which drew vertical streaks down a lit side.
const DOME_EDGE: f32 = 0.6;
const DOME_POWER: f32 = 0.7;
/// The skin the billows carve (design §2.2, §12.7 item 5, §12.9 item 1): a point's depth inside the envelope is the
/// least of its depth across (0 at the outline, 1 where its footprint noise is DEPTH_ACROSS over the threshold) and
/// its depth down from its column's top (1 at CARVE x Size below it); the base is not carved (its rise keeps it flat).
/// The billow tile must pass `cut` = BILLOW_CUT x (1 - depth) (closing to 1 at the envelope, CUT_OUTLINE, over the
/// mist, MIST_SHARE), BILLOW_CUT from BILLOW_CUT_LEAST at Puffiness 0 by BILLOW_CUT_PUFF more at 1, and the density
/// rises over SPAN past it, SPAN_LEAST at Softness 0 and SPAN_SOFT more at 1 (0.3 at the default 0.4; at 0.4 the
/// sunlit side fell under 1.3 times the shaded one). As the root of the smoothstep (Nubis' crisp profile, the
/// prototype's) the lobes looked the same at 1K, but its endless slope at the cut ended 13 % of the 8K export's
/// sky-edge texels on a step over a fifth of the sky (7.6 % without it, 2.2 % before the billows, the reference
/// 0.1 %): a skin crisper than a texel aliases where the export has no AA. From depth CORE_FROM to 1 it is solid
/// whatever the noise. The carve reaches over most of the cloud (DEPTH_ACROSS 0.6, some 400 m in, and CARVE 0.6, 900 m
/// down at Size 1.5) and closes only over its outer fifth, so the balls stand out of the core by their own radius,
/// inside the envelope (§12.9 item 1: no ball reaches past it): at 0.2 and 0.25, closing over half the depth with SPAN
/// 0.8, the noise moved the surface 37 m across and 80 m down, against lobes of 500 m, and every cloud was its
/// envelope with dents (side views: 1.1 lobes a km, the bare envelope 1.0).
/// A weak cloud reaches its full depth across within ACROSS_OWN times its strength's rise over the threshold (never
/// within less than ACROSS_LEAST of the footprint noise): at DEPTH_ACROSS for all, a satellite standing 0.1 over the
/// threshold never got past a fifth of its depth, the cut there let only the balls' peaks through, and it was a
/// spray of loose balls (the cloud count's median size fell to 1 degree) at 2.5 times the ray's cost.
const DEPTH_ACROSS: f32 = 0.6;
const ACROSS_OWN: f32 = 2.0;
const ACROSS_LEAST: f32 = 0.02;
const CARVE: f32 = 0.6;
const BILLOW_CUT_LEAST: f32 = 0.45;
const BILLOW_CUT_PUFF: f32 = 0.4;
const SPAN_LEAST: f32 = 0.1;
const SPAN_SOFT: f32 = 0.5;
const CORE_FROM: f32 = 0.8;
/// The view's body (design §12.10 item 3, the halos and see-through; §12.9 item 1, the balls stand in the skin of a
/// solid core; task 6c): from depth FILL_FROM to CORE_FROM the cut falls on to -SPAN, so the gaps between the lobes
/// fill toward a core that is solid, and its face is where the noise passes the falling cut, lobed. With the gaps empty
/// down to CORE_FROM, lines of sight crossed whole clouds through them (0.099 of the 2K silhouettes under 0.9 opaque
/// at Cover 0.27, setup 1). From 0.3 the clouds lost their lobes; from 0.5 the sun still showed through the Cover 0.2
/// cloud (0.0022 of it).
const FILL_FROM: f32 = 0.4;
/// The view's base is filled too, over the lowest BASE_FILL of the cloud's own height, ramped in over MIST_RAMP of the
/// depth across, so each cloud stands on one flat base (design §2.2, §12.1 item 3 for the march's rows): with its gaps open there, a column's lowest cloud
/// was its lowest lobe, and the bases spread 68 m against the tops' 190 m (`each_type_holds_the_rules` holds them under
/// a quarter; 42 m now). Ramped in over CUT_OUTLINE, its rim was a thin flat skirt that the march found in some rows of
/// texels and not the next: 14 % of the 8K export's sky-edge texels ended on a step over a fifth of the sky (8.9 %
/// over MIST_RAMP; task 6c, Cover 0.27, setup 1).
const BASE_FILL: f32 = 0.1;
/// The depth down is also the point's depth under its own cloud's top as a share of that cloud's height, times
/// DOWN_OWN (design §12.9 item 3): a small cloud a fifth of the shell high never reached the core's depth by CARVE's
/// scale alone and was all skin and mist.
const DOWN_OWN: f32 = 2.0;
/// The cut closes at the envelope (task 6a tune round 2): over the outer CUT_OUTLINE of the depth it rises to 1, the
/// noise's top (its tile is stretched to 0-1), as (1 - depth / CUT_OUTLINE)^2 over BILLOW_CUT x (1 - depth), so no lobe
/// reaches the envelope and the density falls to 0 there. With the cut at BILLOW_CUT on the envelope a lobe met it at
/// up to SPAN's density and stopped there: a ray grazing a cloud's top ran a hundred metres inside it a texel below one
/// that missed it, and 19 % of the 8K export's sky-edge texels ended on a step over a fifth of the sky (the reference
/// 0.1 %). Over the outer fifth (§12.9 item 1): over half the depth it emptied the billows' outer half.
const CUT_OUTLINE: f32 = 0.2;
/// The sun's field (`VolumeShape::extinction`: what the sun-depth grid, its live steps and tau_up read) is the view's,
/// and inside the envelope never under MIST_SHARE of the full one, the mist between the lobes (design §2.2's density,
/// §12.8 item 2's field for the sun read, task 6a tune round 2): from 0 at the envelope's top and outline over MIST_RAMP
/// of the depth, and from all of it below MIST_FADE's first share of the cloud's own height to none above its second.
/// The mist stands for the skin's mean density to a sun grid of 47 m cells, which cannot follow the gaps between lobes
/// finer than that (design §12.10 item 1): without it the light gate's six-sun pixel p99 rose 45 % (0.0547 to 0.0795).
/// The sun's field is never under the view's: as the lobes, the core and the mist alone it missed the view's fill
/// (FILL_FROM, BASE_FILL), so the sun crossed cloud the view draws solid as mist or air and lit its shaded sides and its
/// base rims (the sunlit-halves test's texels toward the sun 2.31 against 1.75 now; the 6c review's S1). The view does
/// not see the mist (task 6c): it drew a translucent veil a few degrees wide round the clouds and ghost lobes through
/// it, and a cloud at Cover 0.2 passed 0.0086 of the sun along the sun's own direction (its base lobes, then 300 m of
/// mist at 0.03-0.15 of full), so the sun's disc showed through it as a white dot.
const MIST_SHARE: f32 = 0.15;
const MIST_RAMP: f32 = 0.5;
const MIST_FADE: (f32, f32) = (0.3, 0.7);
/// The volume footprint's part in the quantile tables' fingerprints (the sheet's is 0).
const FOOTPRINT_PART: u32 = 1;
/// The recipe's id in the footprint grid's key.
const CUMULUS_RECIPE: f32 = 3.0;
/// The profile's tables (design §12.1 item 4): a column's top through the shell over TOP_COLS values of the column
/// channel, and the base's rise over RISE_ROWS heights through the rise's dome.
const TOP_COLS: usize = 33;
const RISE_ROWS: usize = 1024;
/// The base's rise is read through the dome a cloud would have at the footprint share RISE_SHARE, whatever the Cover
/// (never under RISE_DOME_LEAST): a small cloud's base rises over as few metres as its height asks (cumulus humilis
/// keeps a sharp, flat base), and a Cover drag never reshapes a base. Through the Cover's own dome the rise thinned a
/// base as Cover rose (an overhead cloud went from 0.25 to 0.03 opaque as Cover went from 0.5 to 0.9), and through
/// the shell's height alone a small cloud's whole height was its rise (0.25 to 0.006 at Cover 0.5). 0.4 is about the
/// share a Cumulus row's middle Covers solve to (0.35 to 0.5).
const RISE_SHARE: f32 = 0.4;
const RISE_DOME_LEAST: f32 = 0.1;
/// A column's top through the shell runs from TOP_LOW (column 0) to the shell's top (column 1); Towers raises the
/// share of tall ones (design §2.2's top = mix(0.4, 1, column)^towers). Times the dome, a cloud is widest at its base
/// and rounds to its top (design §2.4's radius-by-height curve). At 0.3 with a column that sat low over 43 % of the
/// footprint, half the clouds were 300 m pucks (§12.7 item 5).
const TOP_LOW: f32 = 0.45;
/// Flat base: the rise from the base, 1 - exp(-h01 / sharpness), from BASE_SOFT at Flat base 0 to BASE_FLAT at 1,
/// geometrically (0.098 at the default 0.7, twice design §2.2's 5 %; task 6a tune round 2). With the mist and the cut
/// closing at the envelope, at 0.049 the bases' edges drew in 17 texels at 8K against the reference's 19, at 0.098 in
/// 21 (0.147: 23), and the light gate's pixel error at sun 30 (azimuth 200) held its lock only at 0.098 (0.025-0.027
/// against 0.022 at 0.049, whatever the mist, the floor or the cut's outline). It costs a released ray about 20 %.
/// On the billows (design §12.9 item 7, §12.8 item 11's named number) a rise of 0.05 (sharpness 0.0502, BASE_FLAT
/// 0.0206) was tried once: the light gate's renderer p99 at sun 60, azimuth 200 went from 0.0209 to 0.0339, +62 %,
/// over the 25 % §12.9 item 6 allows (its mean +36 %), so the rise stays 0.098.
const BASE_SOFT: f32 = 0.4;
const BASE_FLAT: f32 = 0.054;
/// Puffiness: the detail erosion's depth (design §2.2's table), only on the skin (§12.7 item 5): full where the shape
/// noise is at the cut, gone ERODE_SPAN past it, and fading out between ERODE_DEPTH's depths inside the envelope, so
/// it never opens a window through a core (eroding wherever the density was under one half emptied low-noise pockets
/// inside bodies and cut a hole through one).
const EROSION_LEAST: f32 = 0.2;
const EROSION_MOST: f32 = 0.6;
const ERODE_SPAN: f32 = 0.15;
const ERODE_DEPTH: (f32, f32) = (0.4, 0.6);
/// The erosion keeps ERODE_KEEP of the density it erodes (task 6a tune round 2): it subtracts, so where a lobe's skin is
/// thinner than the erosion it cut the density to nothing within a few metres, and a ray grazing the skin went from
/// cloud to sky in one texel (with the cut closing at the envelope and no floor, 12 % of the 8K export's sky-edge
/// texels still ended on a step over a fifth of the sky; with it, 2 %, and with the mist 0.2 %). Over the whole height:
/// fading it out over the upper half kept those steps on the tops.
const ERODE_KEEP: f32 = 0.3;
/// Lean: the cloud stands downwind by Lean x this x the shell's depth at its top.
const LEAN_SHEAR: f32 = 1.0;
/// Cover's solve: at most SOLVE_PASSES views of the upper sky, done within SOLVE_WITHIN of Cover.
const SOLVE_PASSES: usize = 5;
const SOLVE_WITHIN: f32 = 0.01;
/// Cover's k (design §3): COVER_VIEWS views from points within COVER_REACH_M of the viewer, spread over the solid
/// angle above COVER_FROM_DEG, each integrated through the shell in steps of COVER_STEP_M.
const COVER_VIEWS: usize = 512;
const COVER_REACH_M: f32 = 12_000.0;
const COVER_FROM_DEG: f32 = 20.0;
const COVER_STEP_M: f32 = 40.0;
/// In cloud, a sample with no density and no cheap density either moves on PROBE_PATHS mean free paths (at full
/// density, over the footprint there) or the texel's footprint, whichever is longer; after EMPTY_TO_AIR of them the
/// march is back in air. One with cheap density but none left after the detail moves SKIN_STEP of a mean free path
/// and does not count.
const PROBE_PATHS: f32 = 2.0;
const EMPTY_TO_AIR: u32 = 5;
/// The lattice in air is never coarser than this, metres: a thin cloud between two lattice points is not seen at all
/// (on the real Cumulus the budget's 150-200 m left pixels' light off by up to 0.35 against a 2 m march).
const LATTICE_MOST_M: f32 = 40.0;
/// In cloud a run's first step is this share of a mean free path, growing to a whole one over the run's first optical
/// depth: the sunlit skin, where the light changes fastest along the view.
const SKIN_STEP: f32 = 0.5;
/// A sample that finds cloud after an empty one halves the gap between them this many times to find the edge.
const ENTRY_HALVINGS: usize = 4;
/// Below this transmittance in front of a step its sun is the grid's alone (design §12.6 item 4).
const SUN_GRID_PAST: f32 = 0.1;
/// The sun-depth grid's fine level reaches this far round the viewer (design §12.1 item 5).
const FINE_RADIUS_M: f32 = 12_000.0;
/// The march in air looks for an empty pyramid cell this many levels up from the one that bounds its reads.
const SKIP_LEVELS: usize = 5;
/// A path the sun grids skip is tested on pyramid cells this many levels over the finest (about 1 km).
const CLEAR_LEVEL_UP: usize = 2;
/// The sun-depth grid's fine cell is never finer than this, metres (its cell at Size 1.5): over the fixed radii its
/// memory and fill time grow as the cell's inverse square, from 27 MB and 0.2 s on the pool at 47 m to about 2.8 GB
/// at Size 0.3's 9.4 m. Smaller clouds get coarser light, not a stalled preview.
const SUN_CELL_LEAST_M: f32 = 47.0;
/// `cover_toward` marches at the released width.
const COVER_WIDTH: usize = 1024;
/// The density a march reads (the detail's fade, the footprint grid's level, the probe's footprint and the skip's level)
/// is never that of a texel wider than one LAW_WIDTH_LEAST texels to a turn: a held bake (512) marches the released
/// clouds at its own texel centres, its jitter at its own texels (design §12.1, held shows the same clouds). With the
/// held texel's own footprint the detail faded by 2.7 km instead of 5.4, and thin cells that live only where the detail
/// is low went: 1336 of 65536 held texels differed from released by over 0.05 (the jitter alone, 22).
const LAW_WIDTH_LEAST: usize = 1024;
/// The stand-in reads tau_up at this many points along a look.
const STAND_IN_POINTS: usize = 16;
/// The billow tile at its coarsest level, as a share of the classic shape tile (SHAPE_TILE_M x Size): 2 km a km of
/// Size (3 km at Size 1.5), its balls some 750, 375 and 190 m apart (§12.7 item 5, §12.9 item 1).
const VOLUME_SHAPE_TILE: f32 = 1.0 / 3.0;
/// The balls follow the cloud's width (design §12.9 item 1): a cloud whose bump stands `wide` of a full one's over the
/// threshold at RISE_SHARE (the root of 1 - the root of threshold over its strength, the share of its reach it covers)
/// reads the tile at LEVELS scales, each twice the last, level log2(WIDE_FULL / wide): the widest clouds (wide about
/// 0.7) between the tile's own scale and the next, a small one up to four times finer, so a small cloud has several
/// lobes, not one ball (at one scale a 300 m cloud was a pill). With the widest clouds at the tile's own scale (WIDE_FULL 0.7, as
/// the prototype's 3 km tile on wider clouds) this recipe's clouds had two or three lobes and the overhead ones were
/// smooth discs; at 1.0 every cloud, the overhead ones too, is lobed, and at 1.4 loose balls came back.
/// A level reads one tile, and a cloud between two (over LEVEL_BLEND of the way from one to the next) blends both:
/// the noise read at a scale that changes from cloud to cloud is not one noise (a chirp where they meet). Each level
/// reads its own place in the tile (LEVEL_PLACE).
const WIDE_FULL: f32 = 1.0;
const LEVELS: usize = 3;
const LEVEL_BLEND: (f32, f32) = (0.35, 0.65);
const LEVEL_PLACE: [f32; 3] = [0.37, 0.61, 0.23];
/// Each level's scale over the tile's own, 2 to the level.
const LEVEL_SCALES: [f32; LEVELS] = [1.0, 2.0, 4.0];
/// A shape keeps the level by strength in a table of LEVEL_ROWS + 1 rows, read linearly (design §12.1 item 4's tables,
/// cheap first: the root, root and log of `level_at` cost a density read some 10 ns).
const LEVEL_ROWS: usize = 256;
/// The step budget's range (the stack's march_steps).
const STEPS_LEAST: u32 = 16;
const STEPS_MOST: u32 = 96;

/// What a volume is built from: the layer, its type, its effective cover (Cover x the master Cover), the map's seed, its
/// wind's way, the sun and the stack's step budget.
#[derive(Clone, Copy)]
pub struct VolumeSpec<'a> {
    pub layer: &'a CloudLayerParams,
    pub genus: CloudGenus,
    pub cover: f32,
    pub map_seed: u32,
    pub downwind: Vec2f,
    pub sun_dir: Vec3f,
    pub march_steps: u32,
}

/// The Cumulus footprint recipe (design §2.4): its rows that change the noise's spread.
#[derive(Clone, Copy, Debug)]
struct CumulusRecipe {
    clustering: f32,
    streets: f32,
}

impl CumulusRecipe {
    /// The footprint's noise, its column channel, its domain warp (cells along and across) and its strength at `f`
    /// (wind frame, cells) with the streams `s`: a cloud a cell and its satellites, each a soft bump (1 - d^2)^2 over its stretched
    /// reach, the bumps joined as their BLEND-norm, on a domain warped once; Clustering's broad mask scales the big
    /// ones and Streets' rows all of them. The column is the cells' values, weighted round their middles. The lattice
    /// stands off the viewer by a seeded fraction of a cell (design §12.9 item 2): at none the viewer sat where four
    /// cells meet, the plane's emptiest point, at every seed (the sky over 62 degrees held 0.05 of cloud at Cover 0.35
    /// against the 0.39 its upper sky hides).
    fn at(&self, f: Vec2f, s: &Streams) -> Node {
        let shift = |salt: u32| (hash_u32(s.0[3] ^ salt) >> 8) as f32 * (1.0 / 16_777_216.0);
        let f = vec2f(f.x + shift(0x51ed_270b), f.y + shift(0x2c1b_3c6d));
        let wx = fbm2(f.x * WARP_FREQ + 3.1, f.y * WARP_FREQ + 7.4, 2, s.0[5]) - 0.5;
        let wz = fbm2(f.x * WARP_FREQ + 9.2, f.y * WARP_FREQ + 1.3, 2, s.0[6]) - 0.5;
        let w = vec2f(f.x + 2.0 * WARP * wx, f.y + 2.0 * WARP * wz);
        // Clustering: the cell's own cloud stands less at random in its cell (jitter 1 - Clustering) and the big ones
        // gather under a broad mask, as the sheet's. The satellites stand at least SATELLITE_JITTER at random whatever
        // Clustering (their seeds are their own, so the cell's cloud does not move): at Clustering 1 all four bumps
        // stood on the cell's middle, one cloud, no size spread (design §12.9 item 3). Nor does the mask scale them: a
        // weak cloud under it came and went with it, and the field's autocorrelation ran out to 5 km at Size 1.5
        // (design §2.4's one cloud a cell of Size).
        let jitter = 1.0 - self.clustering;
        let mut gather = 1.0f32;
        if self.clustering > 0.0 {
            gather = 1.0 - self.clustering + self.clustering * smoothstep(0.35, 0.65, fbm2(f.x * CLUSTER_FREQ, f.y * CLUSTER_FREQ, 2, s.0[2]));
        }
        let rows = if self.streets > 0.0 { 1.0 - self.streets * STREET_DEPTH * (1.0 - wave(f.y / 3.0)) } else { 1.0 };
        let (ix, iy) = (w.x.floor() as i32, w.y.floor() as i32);
        let (mut powers, mut strengths, mut columns, mut weights) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for cy in iy - 1..=iy + 1 {
            let row = hash_u32(s.0[3] ^ (cy as u32).wrapping_mul(0x9e37_79b9));
            for cx in ix - 1..=ix + 1 {
                let h = hash_u32(row ^ (cx as u32).wrapping_mul(0x85eb_ca6b));
                let unit = |k: u32| (hash_u32(h ^ k.wrapping_mul(0x27d4_eb2f)) >> 8) as f32 * (1.0 / 16_777_216.0);
                // The cell's cloud (its hashes as before the satellites came) and its satellites, each with its own.
                for k in 0..=SATELLITES {
                    let own = hash_u32(h ^ k.wrapping_mul(0x632b_e5ab));
                    let unit = |j: u32| if k == 0 { unit(j) } else { (hash_u32(own ^ j.wrapping_mul(0x27d4_eb2f)) >> 8) as f32 * (1.0 / 16_777_216.0) };
                    let jitter = if k == 0 { jitter } else { jitter.max(SATELLITE_JITTER) };
                    let centre = vec2f(cx as f32 + 0.5 + jitter * (unit(1) - 0.5), cy as f32 + 0.5 + jitter * (unit(2) - 0.5));
                    // A stretch along the wind of 1/STRETCH to STRETCH, its root on each axis.
                    let root = (1.0 / STRETCH.sqrt()) * (1.0 + (STRETCH - 1.0) * unit(3));
                    let reach = CELL_REACH * (REACH_LEAST + (1.0 - REACH_LEAST) * unit(4));
                    let (dx, dy) = ((w.x - centre.x) / (reach * root), (w.y - centre.y) * root / reach);
                    let d2 = dx * dx + dy * dy;
                    if d2 < 1.0 {
                        let strength = rows * if k == 0 { gather * (STRENGTH_LEAST + (1.0 - STRENGTH_LEAST) * unit(5)) } else { SATELLITE_STRENGTH.0 + (SATELLITE_STRENGTH.1 - SATELLITE_STRENGTH.0) * unit(5) };
                        // The bump to the power BLEND (2.5): b^2 times its root.
                        let b = (1.0 - d2) * (1.0 - d2) * strength;
                        let power = b * b * b.sqrt();
                        powers += power;
                        strengths += power * strength;
                    }
                }
                // The column: the cell's value, weighted round the cell's middle (not its jittered cloud, so every
                // point has a weight and the window never drops one that is not 0).
                let (ox, oy) = (w.x - (cx as f32 + 0.5), w.y - (cy as f32 + 0.5));
                let r2 = (ox * ox + oy * oy) * (1.0 / (COLUMN_REACH * COLUMN_REACH));
                if r2 < 1.0 {
                    let weight = (1.0 - r2) * (1.0 - r2) * (1.0 - r2) * (1.0 - r2);
                    columns += weight * unit(6);
                    weights += weight;
                }
            }
        }
        let n = powers.powf(1.0 / BLEND);
        [n.clamp(0.0, 1.0), columns / weights, w.x - f.x, w.y - f.y, (strengths + STRENGTH_CLEAR) / (powers + STRENGTH_CLEAR)]
    }

    /// The fields its quantile tables are measured for (design §3.1's fingerprint).
    fn fields(&self) -> [f32; 2] {
        [self.clustering, self.streets]
    }
}

/// A volume's footprint recipe: the Cumulus' (design §2.4) or a type's own (cloud_volume_altocumulus.rs).
#[derive(Clone, Copy, Debug)]
enum VolumeRecipe {
    Cumulus(CumulusRecipe),
    Altocumulus(crate::cloud_volume_altocumulus::AltocumulusRecipe),
}

impl VolumeRecipe {
    fn at(&self, f: Vec2f, s: &Streams) -> Node {
        match self {
            VolumeRecipe::Cumulus(recipe) => recipe.at(f, s),
            VolumeRecipe::Altocumulus(recipe) => recipe.at(f, s),
        }
    }

    fn fields(&self) -> [f32; 2] {
        match self {
            VolumeRecipe::Cumulus(recipe) => recipe.fields(),
            VolumeRecipe::Altocumulus(recipe) => recipe.fields(),
        }
    }
}

/// The rows a volume's shape is built from, its type's block read onto the Cumulus' (an Altocumulus':
/// cloud_volume_altocumulus.rs `rows`).
struct VolumeRows {
    size_km: f32,
    base_m: f32,
    tops_m: f32,
    puffiness: f32,
    flat_base: f32,
    towers: f32,
    lean: f32,
    softness: f32,
    density: f32,
    recipe: VolumeRecipe,
    /// The recipe's id and its rows in the footprint grid's key.
    recipe_key: [f32; 3],
}

/// The billows' level of a cloud of `strength` at the footprint's `threshold` (design §12.9 item 1): how wide its bump
/// stands over the threshold, `wide` (the root of 1 - the root of threshold over strength), as log2(WIDE_FULL / wide),
/// from 0 to LEVELS - 1.
fn level_at(strength: f32, threshold: f32) -> f32 {
    let top = (LEVELS - 1) as f32;
    let wide = (1.0 - (threshold / strength.max(threshold).max(1.0e-6)).sqrt()).max(0.0).sqrt();
    if wide > 0.0 {
        (WIDE_FULL / wide).log2().clamp(0.0, top)
    } else {
        top
    }
}

/// A column's top through the shell (design §2.2): from TOP_LOW at column 0 to the shell's top at 1, Towers raising
/// the share of tall ones (column^p, p from 4 at Towers 0 to 1/4 at 1).
fn top_of(column: f32, towers: f32) -> f32 {
    let p = (2.0f32).powf(2.0 - 4.0 * towers.clamp(0.0, 1.0));
    TOP_LOW + (1.0 - TOP_LOW) * column.clamp(0.0, 1.0).powf(p)
}

/// The recipe at a ground point (metres east and south): noise, column and the warp turned back onto the plane, in
/// metres (what the footprint grid holds).
fn recipe_m(recipe: &VolumeRecipe, frame: &WindFrame, streams: &Streams, cell_m: f32, x: f32, z: f32) -> Node {
    let [n, column, along, across, strength] = recipe.at(frame.of(vec2f(x / cell_m, z / cell_m)), streams);
    let warp = frame.to_plane(vec2f(along, across)) * cell_m;
    [n, column, warp.x, warp.y, strength]
}

/// What the footprint grid gives under a point (`VolumeShape::footprint`): the footprint after the cover remap, the
/// column, the ground point read (shifted upwind by the lean), the cloud's height there as a share of its column's
/// (the dome), the dome its base rises through, the depth across from its outline (0 to 1), the domain warp
/// (metres) and its cloud's strength.
#[derive(Clone, Copy)]
struct Ground {
    f: f32,
    column: f32,
    q: Vec2f,
    dome: f32,
    rise_dome: f32,
    across: f32,
    warp: Vec2f,
    strength: f32,
}

/// smoothstep, local (as in the other cloud modules).
#[inline]
fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Where a march spent its steps (`VolumeShape::walk`): lattice points read in air, moves over empty pyramid cells,
/// steps back on entering cloud, samples in cloud with no density, and samples with density (lit in a lit march).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WalkCounts {
    pub air: u32,
    pub skips: u32,
    pub backs: u32,
    pub empty: u32,
    pub cloud: u32,
}

/// One sample of a march with density (`VolumeShape::walk`): its distance, point and height through the shell, its
/// extinction with detail (1/m) and the sun's field's there (`VolumeShape::extinction`, what the sun reads), the step's
/// length, and whether the sample before it was one too
/// (`gap` metres back). A run's exit is a sample with no extinction (`sigma` 0, `ds` 0) that follows the run's last
/// one: the step into it is the run's last.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub t: f32,
    pub p: Vec3f,
    pub h01: f32,
    pub sigma: f32,
    pub here: f32,
    pub ds: f32,
    pub follows: bool,
    pub gap: f32,
    /// The depth over the point its type's own dome reads (`Cheap::up`), None where the dome reads the tau_up grid.
    pub up: Option<f32>,
}

/// What `VolumeShape::cheap` reads at a point (design §2.2, §12.7 item 5, cheap first).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Cheap {
    /// The view's density without detail, 0 to 1.
    pub(crate) view: f32,
    /// The point's height through the shell.
    pub(crate) h01: f32,
    /// The ground point the lean reads (the detail reads there too).
    pub(crate) q: Vec2f,
    /// The footprint there, which no density at the point exceeds.
    pub(crate) f: f32,
    /// The skin's weight for the erosion (0 where the detail does not erode).
    pub(crate) skin: f32,
    /// The sun's field's density (`VolumeShape::extinction` over sigma): the view's, never under the mist.
    pub(crate) sun: f32,
    /// Inside a cloud's envelope (its footprint, under its top, over its base's rise), where a lobe can stand
    /// between two reads however far under the cut the noise is at this one (design §12.1 item 3).
    pub(crate) envelope: bool,
    /// The depth over the point its type's own dome reads (`cloud_volume_light::step`'s tau_up): a Stratocumulus' its
    /// own column with the sky through its gaps (`ScShape::cheap`), finer than the tau_up grid's 188 m columns, which
    /// average a deck's cells and gaps; None where the dome reads the grid (a Cumulus).
    pub(crate) up: Option<f32>,
}

/// A volume layer's density field, without its light (design §2.2, §3): what the march, the cover and the light's
/// grids read. Immutable; bake threads share it.
pub struct VolumeShape {
    base_m: f32,
    top_m: f32,
    thickness: f32,
    cell_m: f32,
    frame: WindFrame,
    recipe: VolumeRecipe,
    streams: Streams,
    grid: Arc<FootprintGrid>,
    /// The key `grid` is kept under (its GPU copy goes by it, design §12.11 item 4.6).
    grid_key: u64,
    remap: CoverRemap,
    /// The footprint's noise above this is cloud.
    threshold: f32,
    /// Below TAIL_SHARE the remap's gate thins the clouds (it reads the point's cells).
    gated: bool,
    /// The share of the plane the footprint is solved to (c / k), and k.
    share: f32,
    k: f32,
    /// tops[col]: a column's top through the shell at the column channel col / (TOP_COLS - 1).
    tops: Vec<f32>,
    /// rise[row]: the base's rise over the height through the rise's dome (RISE_ROWS rows from 0 to 1), and the
    /// footprint noise's threshold at RISE_SHARE that dome is measured from.
    rise: Vec<f32>,
    rise_threshold: f32,
    /// levels[row]: the billows' level of a cloud of strength row / LEVEL_ROWS at the rise's threshold (`level_at`).
    levels: Vec<f32>,
    noise: Arc<NoiseVolume>,
    /// The billow tile (design §12.9 item 1), read at shape_scale times 2 to the level.
    billow: Arc<Tile3>,
    shape_scale: f32,
    detail_scale: f32,
    /// Where this layer reads the noise tiles (tiles): its own place in them.
    offset: Vec3f,
    detail_texel_m: f32,
    /// The detail noise's mean over its tile: what it fades to.
    detail_mean: f32,
    sigma: f32,
    /// The skin's carve (design §12.7 item 5): the shell's depth over CARVE x Size, BILLOW_CUT and SPAN.
    carve: f32,
    billow_cut: f32,
    span: f32,
    erosion: f32,
    /// Downwind shift at the shell's top, metres (x, z).
    lean: Vec2f,
    steps: u32,
    /// The CPU march's lattice in air is never coarser than this, metres (LATTICE_MOST_M; a deck's own).
    lattice_m: f32,
    cover: f32,
    stream: u32,
    /// The key of every field the extinction reads besides Cover and the seed, Density and sigma included, and the
    /// threshold and share the Cover solve gave (the light grids' caches, design §12.1 item 5).
    key: u64,
    /// Its type, and a Stratus' own numbers (None for a Cumulus).
    genus: CloudGenus,
    stratus: Option<stratus::StratusShape>,
    /// A Stratocumulus' own numbers (its density reads them instead of the Cumulus' carve).
    sc: Option<stratocumulus::ScShape>,
    /// A Nimbostratus' deck (cloud_volume_nimbostratus.rs); None for a Cumulus.
    ns: Option<NsShape>,
}

impl VolumeShape {
    /// The density field of `spec`: None for a type this renderer does not draw yet, no cover or no density. The
    /// footprint grid is looked up or built (its rows spread by `run`, `cancelled` polled); so are the seed's noise
    /// tiles. Cover's share is solved here (design §3).
    pub fn build(spec: &VolumeSpec, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<Option<VolumeShape>, Cancelled> {
        if spec.genus == CloudGenus::Nimbostratus {
            return Self::build_nimbostratus(spec, cancelled, run);
        }
        if spec.genus == CloudGenus::Stratocumulus {
            return stratocumulus::build(spec, cancelled, run);
        }
        if spec.genus == CloudGenus::Stratus {
            return Self::build_stratus(spec, cancelled, run);
        }
        // The type's rows on the shared shape: a Cumulus' own, an Altocumulus' through its own recipe and shell.
        let b = match spec.genus {
            CloudGenus::Cumulus => {
                let b = spec.layer.cumulus_params();
                let recipe = VolumeRecipe::Cumulus(CumulusRecipe { clustering: b.clustering, streets: b.streets });
                VolumeRows { size_km: b.size_km, base_m: b.base_m, tops_m: b.tops_m, puffiness: b.puffiness, flat_base: b.flat_base, towers: b.towers, lean: b.lean, softness: b.softness, density: b.density, recipe, recipe_key: [CUMULUS_RECIPE, b.clustering, b.streets] }
            }
            CloudGenus::Altocumulus => {
                let block = spec.layer.altocumulus_params();
                let r = crate::cloud_volume_altocumulus::rows(&block);
                let recipe = VolumeRecipe::Altocumulus(crate::cloud_volume_altocumulus::AltocumulusRecipe::new(&block));
                VolumeRows { size_km: r.size_km, base_m: r.base_m, tops_m: r.tops_m, puffiness: r.puffiness, flat_base: r.flat_base, towers: r.towers, lean: 0.0, softness: r.softness, density: r.density, recipe, recipe_key: [crate::cloud_volume_altocumulus::RECIPE_ID, block.regularity, block.rows] }
            }
            _ => return Ok(None),
        };
        let c = spec.cover;
        if !(c > 0.0) || !(b.density > 0.0) {
            return Ok(None);
        }
        let cell_m = b.size_km * 1000.0;
        let stream = cloud_layer_seed(spec.map_seed, spec.layer.seed, spec.genus);
        let streams = Streams::of(stream);
        let drift_cells = spec.layer.drift_km.unwrap_or(0.0) * 1000.0 / cell_m;
        let frame = WindFrame::new(spec.downwind, drift_cells);
        let recipe = b.recipe;
        let (base_m, top_m) = (b.base_m, b.tops_m.max(b.base_m + 1.0));
        let thickness = top_m - base_m;
        let lean = spec.downwind * (b.lean * LEAN_SHEAR * thickness);
        // The column's tops, and the base's rise. A cloud's top is its dome times its column's (a lower cloud rounds
        // over lower); the base's rise is read through a dome that does not move with Cover (RISE_SHARE), so a cloud only
        // grows as its dome rises with Cover (design §3: cloud grows from nothing as Cover rises).
        let base_sharpness = BASE_SOFT * (BASE_FLAT / BASE_SOFT).powf(b.flat_base.clamp(0.0, 1.0));
        let rise: Vec<f32> = (0..RISE_ROWS).map(|row| 1.0 - (-(row as f32 / (RISE_ROWS - 1) as f32) / base_sharpness).exp()).collect();
        let tops: Vec<f32> = (0..TOP_COLS).map(|col| top_of(col as f32 / (TOP_COLS - 1) as f32, b.towers)).collect();
        // The tops rise with the column and a read is linear between two of them (the dome is at most 1), so no cloud
        // stands over the top at the column's highest value rounded up to the table; half a metre over it for rounding.
        let highest = tops.clone();
        let top_bound = move |column_hi: f32| {
            let col = ((column_hi.clamp(0.0, 1.0) * (TOP_COLS - 1) as f32).ceil() as usize).min(TOP_COLS - 1);
            base_m + thickness * highest[col] + 0.5
        };
        let bounds = move |_: [f32; 2], column: [f32; 2]| (base_m, top_bound(column[1]));
        // The warp moves where the billow tile is read, not where cloud can stand (that is the footprint's and the
        // column's, which the bounds hold), so it does not widen the margin.
        let margin = SkipMargin { across_m: lean.length() + 1.0, below_m: 0.0, above_m: 0.0 };
        let grid_key = footprint_grid_key(&[b.recipe_key[0], b.size_km, b.recipe_key[1], b.recipe_key[2], spec.downwind.x, spec.downwind.y, drift_cells, b.lean, base_m, top_m, b.towers, b.flat_base], stream, GRID_SIZE, GRID_SPAN_M);
        let at_m = |x: f32, z: f32| recipe_m(&recipe, &frame, &streams, cell_m, x, z);
        let grid = shared_footprint_grid(grid_key, &|| FootprintGrid::build(GRID_SIZE, GRID_SPAN_M, &at_m, &bounds, margin, cancelled, run))?;
        let noise = noise_for(spec.map_seed, cancelled, run)?;
        let billow = noise.billow(cancelled, run)?;
        let place = |k: u32| (hash_u32(stream ^ k.wrapping_mul(0x68e3_1da4)) >> 8) as f32 * (1.0 / 16_777_216.0);
        let size_km = b.size_km;
        let sigma = CUMULUS_SIGMA * b.density;
        let erosion = EROSION_LEAST + (EROSION_MOST - EROSION_LEAST) * b.puffiness;
        let steps = spec.march_steps.clamp(STEPS_LEAST, STEPS_MOST);
        let bits = f32::from_bits;
        let key = footprint_grid_key(&[b.softness, b.puffiness, b.density, sigma, bits(spec.map_seed), bits(grid_key as u32), bits((grid_key >> 32) as u32)], stream, TOP_COLS, base_sharpness);
        let set = quantile_set(fingerprint(spec.genus, FOOTPRINT_PART, &recipe.fields()));
        let mut shape = VolumeShape {
            base_m,
            top_m,
            thickness,
            cell_m,
            frame,
            recipe,
            streams,
            grid,
            grid_key,
            remap: CoverRemap::new(set.clone(), c, FOOTPRINT_EDGE, stream),
            threshold: 0.0,
            gated: false,
            share: c,
            k: 1.0,
            tops,
            rise,
            rise_threshold: 0.0,
            levels: Vec::new(),
            shape_scale: 1.0 / (SHAPE_TILE_M * VOLUME_SHAPE_TILE * size_km),
            detail_scale: 1.0 / (DETAIL_TILE_M * size_km),
            detail_texel_m: DETAIL_TILE_M * size_km / noise.detail.size() as f32,
            detail_mean: noise.detail.mean(),
            noise,
            billow,
            offset: vec3f(place(1), place(2), place(3)),
            sigma,
            carve: thickness / (CARVE * cell_m),
            billow_cut: BILLOW_CUT_LEAST + BILLOW_CUT_PUFF * b.puffiness.clamp(0.0, 1.0),
            span: SPAN_LEAST + SPAN_SOFT * b.softness.clamp(0.0, 1.0),
            erosion,
            lean,
            steps,
            lattice_m: LATTICE_MOST_M,
            cover: c,
            stream,
            key,
            genus: spec.genus,
            stratus: None,
            sc: None,
            ns: None,
        };
        // Cover (design §3): the footprint's share c / k, k what the upper sky's views hide per unit of footprint share,
        // a bracketed secant of at most SOLVE_PASSES views.
        let measured = Streams::of(MEASURE_SEED);
        let n = |_: u32, x: f32, y: f32| recipe.at(vec2f(x, y), &measured)[0];
        shape.rise_threshold = CoverRemap::new(set.clone(), RISE_SHARE, FOOTPRINT_EDGE, stream).threshold(1, &n);
        // The billows' levels are measured at the same share, so a Cover drag never changes a cloud's balls (through
        // the Cover's own threshold a point's density fell as Cover rose, where a cloud's balls changed scale).
        let at = shape.rise_threshold;
        shape.levels = (0..=LEVEL_ROWS).map(|row| level_at(row as f32 / LEVEL_ROWS as f32, at)).collect();
        shape.solve_share(c, &set, &n);
        // The solve reads the views with detail, so Puffiness (and any row the views see) moves the threshold the
        // extinction reads: the light grids' key carries what the solve gave, not the rows that led to it.
        shape.key = footprint_grid_key(&[bits(key as u32), bits((key >> 32) as u32), shape.threshold, shape.share, shape.gated as u32 as f32], stream, TOP_COLS, base_sharpness);
        Ok(Some(shape))
    }

    /// Solves the footprint's share for the upper sky's views to be `c` hidden (design §3): the hidden share rises with
    /// the footprint's share from 0 at 0 (steeply: a cloud's middle is taller than its edge), so a secant between a
    /// share that hides too little and one that hides too much, from c and the share c / k its first view gives, up to
    /// SOLVE_PASSES views; the share that came nearest is kept. The views are the same at every Cover, so the share
    /// follows Cover smoothly.
    fn solve_share(&mut self, c: f32, set: &Arc<crate::cloud_cover::QuantileSet>, n: &dyn Fn(u32, f32, f32) -> f32) {
        let c = c.min(1.0);
        let (mut low, mut high) = ((0.0f32, 0.0f32), (1.0f32, 1.0f32));
        let mut best = (f32::INFINITY, c);
        let mut share = c;
        for _ in 0..SOLVE_PASSES {
            self.set_share(share, set, n);
            let hidden = self.sky_opacity();
            if (hidden - c).abs() < best.0 {
                best = ((hidden - c).abs(), share);
            }
            if (hidden - c).abs() <= SOLVE_WITHIN {
                break;
            }
            if hidden < c {
                low = (share, hidden);
            } else {
                high = (share, hidden);
            }
            // The secant between the bracket's ends, held inside it.
            let (a, b) = (low, high);
            let next = if b.1 > a.1 { a.0 + (c - a.1) * (b.0 - a.0) / (b.1 - a.1) } else { 0.5 * (a.0 + b.0) };
            share = next.clamp(a.0 + 0.05 * (b.0 - a.0), b.0 - 0.05 * (b.0 - a.0));
        }
        self.set_share(best.1, set, n);
        self.k = if best.1 > 0.0 { c / best.1 } else { 1.0 };
    }

    /// Solves the footprint's threshold for the share `share` of the plane.
    fn set_share(&mut self, share: f32, set: &Arc<crate::cloud_cover::QuantileSet>, n: &dyn Fn(u32, f32, f32) -> f32) {
        self.share = share;
        self.remap = CoverRemap::new(set.clone(), share, FOOTPRINT_EDGE, self.stream);
        self.threshold = self.remap.threshold(1, n);
        self.gated = share < crate::cloud_cover::TAIL_SHARE;
    }

    /// The share of COVER_VIEWS views of the upper sky that are over half opaque (design §3's k, per unit of footprint
    /// share), counted softly (smoothstep from 0.3 to 0.7 of each view's opacity) so it is continuous in Cover: from
    /// points within COVER_REACH_M of the viewer, along directions spread over the solid angle above COVER_FROM_DEG,
    /// each integrated through the shell (flat: the curvature is nothing this high) in steps of about COVER_STEP_M,
    /// with detail, at the footprint grid's finest level. The views are the same at every Cover.
    fn sky_opacity(&self) -> f32 {
        let low = COVER_FROM_DEG.to_radians().sin();
        let mut sum = 0.0f32;
        for i in 0..COVER_VIEWS as u32 {
            let unit = |salt: u32| (hash_u32(hash_u32(i ^ 0x2f6b_8a1d) ^ salt) >> 8) as f32 * (1.0 / 16_777_216.0);
            let (x, z) = ((2.0 * unit(1) - 1.0) * COVER_REACH_M, (2.0 * unit(2) - 1.0) * COVER_REACH_M);
            let y = low + (1.0 - low) * unit(3);
            let a = std::f32::consts::TAU * unit(4);
            let r = (1.0 - y * y).max(0.0).sqrt();
            let d = vec3f(r * a.cos(), y, r * a.sin());
            let (t0, t1) = (self.base_m / y, self.top_m / y);
            let n = ((t1 - t0) / COVER_STEP_M).ceil().max(1.0);
            let dt = (t1 - t0) / n;
            let mut tau = 0.0f32;
            for k in 0..n as usize {
                let p = vec3f(x, 0.0, z) + d * (t0 + (k as f32 + 0.5) * dt);
                if let VolumeRecipe::Altocumulus(recipe) = &self.recipe {
                    // an Altocumulus' views see its lens (the GPU's density, through its CPU twin): the Cumulus stand-in on
                    // its thin deck hid far less than the lenses do, and Cover 0.6 hid 0.9 of the sky
                    let q = vec2f(p.x, p.z) / self.cell_m;
                    let ([nf, column, _, _, deck], peak, strength) = recipe.sample(self.frame.of(q), &self.streams);
                    let f = if !(nf > self.threshold) { 0.0 } else if self.gated { self.remap.density(nf, self.threshold, self.frame.of(q)) } else { cover_density(nf, self.threshold, FOOTPRINT_EDGE) };
                    let at = crate::cloud_volume_altocumulus::LensPoint { n: nf, deck, strength, peak, column, f, h01: (p.y - self.base_m) / self.thickness };
                    tau += crate::cloud_volume_altocumulus::lens_density(recipe, &at, self.threshold, self.share, self.thickness, self.sigma) * dt;
                } else {
                    let c = self.cheap(p, 0.0);
                    if c.view > 0.0 {
                        tau += self.full(c.view, c.skin, c.q, p.y, c.h01, 1.0) * self.sigma * dt;
                    }
                }
                if tau > 6.0 {
                    break;
                }
            }
            // A soft count of the views over half opaque (what Cover counts), continuous in the opacity.
            sum += smoothstep(0.3, 0.7, 1.0 - (-tau).exp());
        }
        sum / COVER_VIEWS as f32
    }

    /// What the footprint grid gives under a point at `h01` ([`Ground`]), read at the mip level of `footprint_m`,
    /// shifted upwind by the lean: the dome is DOME_EDGE's rise to the power DOME_POWER, the base's rise dome the same
    /// at RISE_SHARE (its root), the depth across reached within ACROSS_OWN times its cloud's strength over the
    /// threshold, never further in than DEPTH_ACROSS.
    #[inline]
    fn footprint(&self, x: f32, z: f32, h01: f32, footprint_m: f32) -> Ground {
        let q = vec2f(x - self.lean.x * h01, z - self.lean.y * h01);
        let none = |column: f32| Ground { f: 0.0, column, q, dome: 0.0, rise_dome: 1.0, across: 0.0, warp: Vec2f::default(), strength: 1.0 };
        // Outside the grid's span there is no cloud, whatever the lean's shifted read finds: the max pyramid says so.
        if !(x.abs() < self.grid.half_m() && z.abs() < self.grid.half_m()) {
            return none(0.0);
        }
        // (an Altocumulus' cells are a few grid texels wide: its fallback reads the grid's finest level at every width,
        // or a coarse bake's mips blurred the cells' creases and its cover moved with the width)
        let read_m = if self.genus == CloudGenus::Altocumulus { 0.0 } else { footprint_m };
        let [n, column, wx, wz, strength] = self.grid.sample(q.x, q.y, read_m);
        if !(n > self.threshold) {
            return none(column);
        }
        let f = if self.gated { self.remap.density(n, self.threshold, self.frame.of(q / self.cell_m)) } else { cover_density(n, self.threshold, FOOTPRINT_EDGE) };
        let rise_dome = ((n - self.rise_threshold) / DOME_EDGE).clamp(0.0, 1.0).sqrt().max(RISE_DOME_LEAST);
        let dome = ((n - self.threshold) / DOME_EDGE).min(1.0).powf(DOME_POWER);
        // The depth across is reached within ACROSS_OWN times the cloud's own rise over the threshold (or the noise's
        // where clouds overlap over their strength), never further than DEPTH_ACROSS: a weak cloud gets its own core
        // and skin (§12.9 items 1 and 3), and a point's depth only grows as the threshold falls (Cover rises).
        let span = DEPTH_ACROSS.min(ACROSS_OWN * (strength.max(n) - self.threshold)).max(ACROSS_LEAST);
        Ground { f, column, q, dome, rise_dome, across: smoothstep(self.threshold, self.threshold + span, n), warp: vec2f(wx, wz), strength }
    }

    /// The billow tile at the warped ground point `w` and height `y` (metres) in a cloud of `strength` (design §12.9 item
    /// 1): at the level of the cloud's width (WIDE_FULL), one read, or two blended between levels (LEVEL_BLEND).
    #[inline]
    fn billows(&self, w: Vec2f, y: f32, strength: f32) -> f32 {
        let g = strength.clamp(0.0, 1.0) * LEVEL_ROWS as f32;
        let row = (g as usize).min(LEVEL_ROWS - 1);
        let level = self.levels[row] + (self.levels[row + 1] - self.levels[row]) * (g - row as f32);
        let low = level.floor().min((LEVELS - 2) as f32);
        let t = smoothstep(LEVEL_BLEND.0, LEVEL_BLEND.1, level - low);
        let low = low as usize;
        let read = |l: usize| {
            // 2 to the level, from a table (the level is a whole number).
            let scale = self.shape_scale * LEVEL_SCALES[l];
            let place = vec3f(LEVEL_PLACE[0], LEVEL_PLACE[1], LEVEL_PLACE[2]) * l as f32 + self.offset;
            self.billow.sample(w.x * scale + place.x, y * scale + place.y, w.y * scale + place.z)
        };
        if !(t > 0.0) {
            read(low)
        } else if !(t < 1.0) {
            read(low + 1)
        } else {
            let a = read(low);
            a + (read(low + 1) - a) * t
        }
    }

    /// The base's rise at `x`, the height through the rise's dome, linear between its table's rows.
    #[inline]
    fn rise_at(&self, x: f32) -> f32 {
        let g = x.clamp(0.0, 1.0) * (RISE_ROWS - 1) as f32;
        let row = (g as usize).min(RISE_ROWS - 2);
        let t = g - row as f32;
        self.rise[row] + (self.rise[row + 1] - self.rise[row]) * t
    }

    /// A column's top through the shell at the column channel `column`, linear in its table.
    #[inline]
    fn top_at(&self, column: f32) -> f32 {
        let g = column.clamp(0.0, 1.0) * (TOP_COLS - 1) as f32;
        let col = (g as usize).min(TOP_COLS - 2);
        self.tops[col] + (self.tops[col + 1] - self.tops[col]) * (g - col as f32)
    }

    /// The density without detail at `p` (0 to 1; design §2.2, §12.7 item 5, cheap first: footprint, top, shape), read
    /// at the footprint grid's level of `footprint_m`, with what the march and the light read beside it (`Cheap`): the
    /// sun's field, the view's with the mist between its lobes, never exceeds the footprint.
    #[inline]
    fn cheap(&self, p: Vec3f, footprint_m: f32) -> Cheap {
        if let Some(ns) = &self.ns {
            return self.ns_cheap(ns, p, footprint_m);
        }
        if let Some(sc) = &self.sc {
            return sc.cheap(self, p, footprint_m);
        }
        if let Some(st) = &self.stratus {
            return self.stratus_cheap(st, p, footprint_m);
        }
        let h01 = (p.y - self.base_m) / self.thickness;
        if !(h01 > 0.0 && h01 < 1.0) {
            return Cheap { h01, ..Cheap::default() };
        }
        let g = self.footprint(p.x, p.z, h01, footprint_m);
        if !(g.f > 0.0) {
            return Cheap { h01, q: g.q, ..Cheap::default() };
        }
        // The envelope: under the column's top times the dome (its edge low, its middle full), the base's rise through
        // its dome at RISE_SHARE.
        let top = g.dome * self.top_at(g.column);
        let air = Cheap { h01, q: g.q, f: g.f, ..Cheap::default() };
        if !(h01 < top) {
            return air;
        }
        let rise = self.rise_at(h01 / g.rise_dome);
        if !(rise > 0.0) {
            return air;
        }
        // The skin: the billow tile on the warped domain must pass a cut that deepens toward the envelope's outline
        // and top and closes at it (CUT_OUTLINE); deep inside the core is solid.
        let down = ((top - h01) * self.carve).max((top - h01) / top * DOWN_OWN);
        let depth = down.min(g.across);
        let outline = (1.0 - depth / CUT_OUTLINE).max(0.0);
        let cut = self.billow_cut * (1.0 - depth) + (1.0 - self.billow_cut) * outline * outline;
        let w = g.q + g.warp;
        let s = self.billows(w, p.y, g.strength);
        // The view's: the lobes on a body that fills toward the core (FILL_FROM), solid from CORE_FROM, and over its base
        // (BASE_FILL).
        let base = (1.0 - smoothstep(0.0, BASE_FILL, h01 / top)) * smoothstep(0.0, MIST_RAMP, g.across);
        let cut = cut - (cut + self.span) * smoothstep(FILL_FROM, CORE_FROM, depth).max(base);
        let billow = smoothstep(cut, cut + self.span, s);
        // The sun's: the view's, never under the mist between its lobes (MIST_SHARE), so the sun crosses all the cloud
        // the view draws (design §12.8 item 2: the sun's field differs only by the skin's sub-grid mean).
        let mist = MIST_SHARE * smoothstep(0.0, MIST_RAMP, down) * smoothstep(0.0, MIST_RAMP, g.across) * (1.0 - smoothstep(MIST_FADE.0, MIST_FADE.1, h01 / top));
        let sun = billow.max(mist);
        if !(sun > 0.0) {
            return Cheap { envelope: true, ..air };
        }
        let skin = (1.0 - smoothstep(0.0, ERODE_SPAN, s - cut)) * (1.0 - smoothstep(ERODE_DEPTH.0, ERODE_DEPTH.1, depth));
        Cheap { view: billow * g.f * rise, h01, q: g.q, f: g.f, skin, sun: sun * g.f * rise, envelope: true, up: None }
    }

    /// The density with detail from the cheap one `cheap` and its skin weight `skin` (design §2.2, §12.1 item 4,
    /// §12.7 item 5): the detail erodes it only on the skin, wispy low and billowy high, never under ERODE_KEEP of it. `detail` is how much of the
    /// detail noise shows (1 near, fading to 0 where a detail texel is finer than the texel's footprint): where it
    /// fades the noise fades to its mean, not to nothing, so the erosion's mean stays and a volume hides the same share
    /// of the sky at every bake width (design §3.4; fading the erosion out made a 512 bake's clouds 0.04 wider than a
    /// 4096 one's). Never above `cheap`.
    #[inline]
    fn full(&self, cheap: f32, skin: f32, q: Vec2f, y: f32, h01: f32, detail: f32) -> f32 {
        if !(skin > 0.0) {
            return cheap;
        }
        let fine = if detail > 0.0 {
            let fine = self.noise.detail.sample(q.x * self.detail_scale + self.offset.z, y * self.detail_scale + self.offset.x, q.y * self.detail_scale + self.offset.y);
            self.detail_mean + (fine - self.detail_mean) * detail.min(1.0)
        } else {
            self.detail_mean
        };
        let fine = fine + (1.0 - 2.0 * fine) * (4.0 * h01).min(1.0);
        let e = self.erosion * fine * skin;
        ((cheap - e) / (1.0 - e)).max(ERODE_KEEP * cheap)
    }

    /// The extinction (1/m) without detail at `p`, at the footprint grid's finest level: what the sun's grids and live
    /// steps read (design §12.1 item 5). 0 outside the shell.
    #[inline]
    pub fn extinction(&self, p: Vec3f) -> f32 {
        if let Some(sc) = &self.sc {
            return sc.sun_field(self, p) * self.sigma;
        }
        self.cheap(p, 0.0).sun * self.sigma
    }

    /// Whether the straight path from `a` to `b` meets no cloud: the footprint's max pyramid (CLEAR_LEVEL_UP levels over
    /// its finest) says each cell it crosses has no footprint, or the path's heights there miss the cell's base and
    /// top (design §12.6 item 6). Exact: a cell bounds every read inside it, the lean's shift included.
    pub fn span_is_clear(&self, a: Vec3f, b: Vec3f) -> bool {
        let level = self.grid.first_bounds_level() + CLEAR_LEVEL_UP;
        let (o, v) = (vec2f(a.x, a.z), vec2f(b.x - a.x, b.z - a.z));
        let rise = b.y - a.y;
        let threshold = self.threshold;
        self.grid.cells_along(level, o, v, 0.0, 1.0, &mut |cell, t0, t1| {
            if !(cell.footprint > threshold) {
                return true;
            }
            let (y0, y1) = (a.y + rise * t0, a.y + rise * t1);
            y0.max(y1) < cell.base_m || y0.min(y1) > cell.top_m
        })
    }

    /// The march along the unit direction `d` (d.y > 0) at a bake `width` texels wide (its jitter; its density by
    /// [`law_angle`]), calling `visit` with every sample that has density, front to back, until it returns false
    /// (design §12.1 items 2-4, 8). `skip` crosses empty
    /// pyramid cells in one move (the plain walk without it reads the same samples). Returns where its steps went.
    pub fn walk(&self, d: Vec3f, width: usize, skip: bool, visit: &mut dyn FnMut(&Sample) -> bool) -> WalkCounts {
        let mut counts = WalkCounts::default();
        let Some((near, far)) = self.ray_span(d) else {
            return counts;
        };
        // The lattice: half the budget overhead, all of it toward the horizon (design §2.1), at most LATTICE_MOST_M a
        // step, offset by the texel's blue noise.
        let steps = (self.steps as f32 * (1.0 - 0.5 * d.y)).round().max(8.0);
        let steps = steps.max(((far - near) / self.lattice_m).ceil());
        let ds = (far - near) / steps;
        let width = width.max(2);
        let uv = ibl::dir_to_equirect_uv(arr(d));
        let (ix, iy) = ((uv[0] * width as f32) as u32, (uv[1] * (width / 2) as f32) as u32);
        let start = near + blue_jitter(ix, iy) * ds;
        // A texel's footprint per metre along the ray, by the density's law: never wider than a released texel.
        let angle = law_angle(width);
        let mfp = 1.0 / self.sigma;
        let v = vec2f(d.x, d.z);
        let point = |t: f32| vec3f(d.x * t, height_at(t, d.y), d.z * t);
        // The pyramid level that bounds every read of this ray (its footprint grows with distance).
        let skip_from = self.grid.level_for(far * angle).max(self.grid.first_bounds_level());
        let (mut k, mut t_done, mut back_from) = (0i64, near, -1i64);
        'air: loop {
            let t = start + k as f32 * ds;
            if t > far {
                break;
            }
            let p = point(t);
            if skip {
                if let Some(end) = self.empty_until(skip_from, v, t, p.y, d.y) {
                    counts.skips += 1;
                    k = (((end - start) / ds).ceil() as i64).max(k + 1);
                    continue;
                }
            }
            counts.air += 1;
            // Cloud is looked for by the envelope, not by a density inside it (design §12.1 item 3, §12.9 item 1:
            // the lobes live in the envelope's skin): a lattice point in a gap between lobes starts the walk in
            // cloud, whose short probes find the thin lobes and base wedges round it. Looked for by the view's
            // density alone, a lattice point in a gap missed them: 40 % of the sky-edge texels at el 40-55 of the 8K
            // export ended on a step over a fifth of the sky, dotted along the bases' rims (task 6c, Cover 0.27); by
            // the mist too, they were still missed over each cloud's upper 30 %, where the mist is 0 (MIST_FADE).
            if !self.cheap(p, t * angle).envelope {
                k += 1;
                continue;
            }
            // Cloud: one lattice step back (unless this lattice point already brought the march back), never behind
            // what was walked (the first lattice point goes back to where the ray enters the shell: the blue-noise
            // start must not cut a base off); then about a mean free path a step until EMPTY_TO_AIR empty samples past
            // this point.
            let mut tc = if back_from != k {
                back_from = k;
                counts.backs += 1;
                (start + (k - 1) as f32 * ds).max(t_done)
            } else {
                t.max(t_done)
            };
            let (mut empties, mut follows, mut gap, mut last_empty, mut last_lit) = (0u32, false, 0.0f32, None, 0.0f32);
            let mut run_tau = 0.0f32;
            let at = |t: f32| self.on_ray(d, angle, t);
            loop {
                if tc > far {
                    break 'air;
                }
                let (mut full, mut c, mut p) = at(tc);
                // Entering cloud from an empty sample: the edge is found by halving the gap (design §12.1 item 3:
                // the sunlit skin is a mean free path deep, so where the first sample lands must not set its light).
                if full > 0.0 {
                    if let Some(mut low) = last_empty {
                        let mut high = tc;
                        for _ in 0..ENTRY_HALVINGS {
                            let mid = 0.5 * (low + high);
                            if at(mid).0 > 0.0 {
                                high = mid;
                            } else {
                                low = mid;
                            }
                        }
                        if high < tc {
                            tc = high;
                            (full, c, p) = at(tc);
                        }
                    }
                }
                if full > 0.0 {
                    last_empty = None;
                    counts.cloud += 1;
                    let sigma = full * self.sigma;
                    // About a mean free path of the most density the footprint here allows (design §12.1 item 3):
                    // a thin edge's own density would send the step deep into the cloud past its sunlit skin.
                    if !follows {
                        run_tau = 0.0;
                    }
                    // A mean free path at full density (the footprint here does not bound the density a step ahead),
                    // from SKIN_STEP of one at a run's first sample up to a whole one over its first optical depth: the
                    // sunlit skin is where the light changes fastest along the view (design §12.1 item 3).
                    let ds_cloud = (mfp * (SKIN_STEP + (1.0 - SKIN_STEP) * run_tau.min(1.0))).min(ds);
                    run_tau += sigma * ds_cloud;
                    if !visit(&Sample { t: tc, p, h01: c.h01, sigma, here: c.sun * self.sigma, ds: ds_cloud, follows, gap, up: c.up }) {
                        break 'air;
                    }
                    follows = true;
                    gap = ds_cloud;
                    empties = 0;
                    last_lit = tc;
                    tc += ds_cloud;
                } else {
                    counts.empty += 1;
                    // A run ends: its exit is found by halving between its last lit sample and this empty one (the
                    // entry's rule mirrored) and handed on as a sample with no extinction that follows the run, so the
                    // run's last step is a trapezoid down to 0 there, not a whole step at its last sample's extinction:
                    // that step moved with the run's own comb, a sample at a time, and drew terraces (design §12.1
                    // item 3).
                    if follows {
                        let (mut low, mut high, mut exit) = (last_lit, tc, (c.sun, c.h01, p));
                        for _ in 0..ENTRY_HALVINGS {
                            let mid = 0.5 * (low + high);
                            let read = at(mid);
                            if read.0 > 0.0 {
                                low = mid;
                            } else {
                                (high, exit) = (mid, (read.1.sun, read.1.h01, read.2));
                            }
                        }
                        if !visit(&Sample { t: high, p: exit.2, h01: exit.1, sigma: 0.0, here: exit.0 * self.sigma, ds: 0.0, follows: true, gap: high - last_lit, up: None }) {
                            break 'air;
                        }
                    }
                    follows = false;
                    last_empty = Some(tc);
                    // Inside the envelope the full density can begin anywhere (an eroded wisp shorter than a long
                    // probe was found only where a probe landed in it, and its outline was cut where it got shorter
                    // than the stride; a lobe stands in any gap between two): the probe moves SKIN_STEP of a mean free
                    // path, or the texel's footprint, and is not counted toward air. Outside it the long stride by the
                    // footprint here, read from the grid under a point below the shell's base too (design §12.1 items
                    // 3 and 4).
                    if c.envelope {
                        tc += (SKIN_STEP * mfp).max(tc * angle).min(ds);
                    } else {
                        empties += 1;
                        let f = if c.h01 > 0.0 { c.f } else { self.footprint(p.x, p.z, 0.0, tc * angle).f };
                        tc += (PROBE_PATHS * mfp / f.max(1.0e-3)).max(tc * angle).min(ds);
                    }
                    if empties >= EMPTY_TO_AIR && tc >= t {
                        t_done = tc;
                        k = (((tc - start) / ds).ceil() as i64).max(k + 1);
                        continue 'air;
                    }
                }
            }
        }
        counts
    }

    /// For each texel of the upper half of a map `width` wide (row by row), whether the walk's read where its centre's
    /// ray enters the shell lies inside it: `cheap`'s h01 over 0 at `ray_span`'s near, the march's first read wherever
    /// its first lattice point stands in a cloud's envelope. That read sits on the base to the rounding of the
    /// closed-form crossing, so it is a coin flip of the last bit, and taken one way it is a sample of next to no
    /// density whose short step finds a sliver the other way's long probe stride steps over: the GPU, whose rounding is
    /// not the CPU's, takes the CPU's flip from here (design §12.3: held to the CPU by tolerance). 24 texels a float
    /// (exact integers), bit i % 24 of float i / 24. The texel's direction is `LightRecord::read`'s, normalised as
    /// `CloudVolume::march` takes it; an antialiasing bake's (`jitter` in texels, design §12.1 item 10) is the jittered
    /// bake's (`Env::bake_rows_jittered_par`: x + 0.5 + jx, the centre's bits at 0).
    pub(crate) fn inside_at_near_bits(&self, width: usize, jitter: [f32; 2]) -> Vec<f32> {
        let width = width.max(2);
        let height = (width / 2).max(1);
        let rows = height.div_ceil(2);
        let [jx, jy] = jitter;
        let columns: Vec<(f32, f32)> = (0..width)
            .map(|x| {
                let phi = ((x as f32 + 0.5 + jx) / width as f32 - 0.5) * std::f32::consts::TAU;
                (phi.sin(), phi.cos())
            })
            .collect();
        let mut words = vec![0u32; (width * rows).div_ceil(24)];
        for y in 0..rows {
            let (st, ct) = (((y as f32 + 0.5 + jy) / height as f32) * std::f32::consts::PI).sin_cos();
            for (x, &(sp, cp)) in columns.iter().enumerate() {
                let d = vec3f(st * sp, ct, -st * cp);
                let d = d / d.length();
                let inside = self.ray_span(d).is_some_and(|(near, _)| {
                    let h01 = (height_at(near, d.y) - self.base_m) / self.thickness;
                    h01 > 0.0 && h01 < 1.0
                });
                if inside {
                    let i = y * width + x;
                    words[i / 24] |= 1 << (i % 24);
                }
            }
        }
        words.into_iter().map(|w| w as f32).collect()
    }

    /// Where the unit direction `d` crosses the shell within the volumes' range: (near, far) metres from the eye, None
    /// where it does not (at and below the horizon too).
    pub(crate) fn ray_span(&self, d: Vec3f) -> Option<(f32, f32)> {
        if !(d.y > 0.0) || !d.is_finite() {
            return None;
        }
        let horizontal = (1.0 - d.y * d.y).max(0.0).sqrt();
        let near = shell_distance(d.y, self.base_m);
        let far = shell_distance(d.y, self.top_m).min(RANGE_M / horizontal.max(1.0e-4));
        (far > near).then_some((near, far))
    }

    /// The density `t` metres along the unit direction `d` as the march reads it at a texel `angle` radians wide:
    /// (with detail, what `cheap` reads there, the point). The detail fades out where a detail texel is finer than the
    /// texel's footprint (design §12.1 item 4).
    #[inline]
    pub(crate) fn on_ray(&self, d: Vec3f, angle: f32, t: f32) -> (f32, Cheap, Vec3f) {
        let p = vec3f(d.x * t, height_at(t, d.y), d.z * t);
        let footprint_m = t * angle;
        let c = self.cheap(p, footprint_m);
        let full = if c.view > 0.0 { self.full(c.view, c.skin, c.q, p.y, c.h01, 1.0 - smoothstep(0.5 * self.detail_texel_m, self.detail_texel_m, footprint_m)) } else { 0.0 };
        (full, c, p)
    }

    /// The extinction (1/m) at density 1: sigma x Density.
    pub fn sigma(&self) -> f32 {
        self.sigma
    }

    /// From the lattice point at `t` (height `y`), the end of the largest empty pyramid cell holding it, from level
    /// `from` up to SKIP_LEVELS above: a cell with no footprint, or one the ray is over the tops of (it only climbs) or
    /// under the bases of until it leaves the cell. A little short of the edge, so a lattice point on it is read.
    #[inline]
    fn empty_until(&self, from: usize, v: Vec2f, t: f32, y: f32, mu: f32) -> Option<f32> {
        let mut best = None;
        let x = (v.x * t, v.y * t);
        for level in from..(from + SKIP_LEVELS).min(self.grid.levels()) {
            let cell = self.grid.bounds_at(level, x.0, x.1);
            let end = self.grid.cell_exit(level, Vec2f::default(), v, t);
            let empty = !(cell.footprint > self.threshold) || y > cell.top_m || height_at(end, mu) < cell.base_m;
            if !empty {
                break;
            }
            best = Some(end);
        }
        best.map(|end| end - 0.01 - 1.0e-6 * end)
    }

    /// The opacity along the unit direction `d` at a bake `width`, no light: the march's walk with each step's
    /// Beer-Lambert (its extinction and its length as the lit march takes them, [`through`] and [`step_length`], a
    /// run's exit only ending the step before it), stopped and counted opaque at T 0.01 as the lit march is, so it is
    /// the lit march's opacity bit for bit. `skip` as `walk`'s.
    pub fn opacity(&self, d: Vec3f, width: usize, skip: bool) -> f32 {
        let mut transmittance = 1.0f32;
        let mut waiting: Option<Sample> = None;
        self.walk(d, width, skip, &mut |s| {
            if let Some(w) = waiting.filter(|w| w.sigma > 0.0) {
                transmittance *= (-(through(&w, s) * step_length(&w, s))).exp();
                if transmittance <= MIN_TRANSMITTANCE {
                    return false;
                }
            }
            waiting = Some(*s);
            true
        });
        if let (true, Some(w)) = (transmittance > MIN_TRANSMITTANCE, waiting.filter(|w| w.sigma > 0.0)) {
            transmittance *= (-(w.sigma * w.ds)).exp();
        }
        if transmittance <= MIN_TRANSMITTANCE {
            1.0
        } else {
            1.0 - transmittance
        }
    }

    /// The shell, metres above the ground.
    pub fn shell_m(&self) -> (f32, f32) {
        (self.base_m, self.top_m)
    }

    /// The footprint's solved share of the plane (c / k) and k (design §3).
    pub fn cover_share(&self) -> (f32, f32) {
        (self.share, self.k)
    }

    /// What its sun-depth grid is built for under `sun` (design §12.1 item 5, §12.6 items 4 and 8): its shell, cells of
    /// its billow tile's own texel at its coarsest level (SUN_CELL_M x VOLUME_SHAPE_TILE x Size: 23.5 m at Size 1.5; a
    /// 70 m cell, the classic tile's texel, left a real Cumulus' light off by up to 0.16 at a 9 degree sun; the finer
    /// levels' balls, down to 47 m apart, are finer than the cell, design §12.9 item 6) but never under
    /// SUN_CELL_LEAST_M,
    /// the fine level out to FINE_RADIUS_M and the coarse to COARSE_RADIUS_M, and a body a third of its full extinction.
    /// A sun that is not one is overhead.
    pub fn sun_grid_spec(&self, sun: Vec3f) -> SunGridSpec {
        let sun = sun.normalize();
        let sun = if sun.is_finite() && sun.length() > 0.5 { sun } else { vec3f(0.0, 1.0, 0.0) };
        SunGridSpec { sun, base_m: self.base_m, top_m: self.top_m, cell_m: (SUN_CELL_M * VOLUME_SHAPE_TILE * self.cell_m / 1000.0).max(SUN_CELL_LEAST_M), fine_radius_m: FINE_RADIUS_M, coarse_radius_m: COARSE_RADIUS_M, body_extinction: self.sigma / 3.0 }
    }

    /// The key of every field its extinction reads besides Cover and the seed (the light grids' caches), its Cover and
    /// its stream (the seed those caches take).
    pub fn light_key(&self) -> (u64, f32, u32) {
        (self.key, self.cover, self.stream)
    }

    /// Its footprint recipe at a ground point (metres east and south), not through the grid: (noise, column, the
    /// domain warp east and south in metres, and its cloud's strength).
    pub fn footprint_recipe(&self, x_m: f32, z_m: f32) -> Node {
        if self.ns.is_some() {
            return nimbostratus::recipe_m(&self.frame, &self.streams, self.cell_m, x_m, z_m);
        }
        if let Some(sc) = &self.sc {
            return sc.recipe_m(self, x_m, z_m);
        }
        if let Some(st) = &self.stratus {
            return st.recipe_m(&self.frame, &self.streams, self.cell_m, x_m, z_m);
        }
        recipe_m(&self.recipe, &self.frame, &self.streams, self.cell_m, x_m, z_m)
    }

    /// The footprint grid it reads.
    pub fn grid(&self) -> &FootprintGrid {
        &self.grid
    }

    /// The footprint after the cover remap at a ground point (metres east and south), from its recipe, not through the
    /// grid (so anywhere on the plane): where it is above 0 a column holds cloud (design §3.1's share of the plane).
    pub fn footprint_at(&self, x_m: f32, z_m: f32) -> f32 {
        let q = vec2f(x_m, z_m);
        let n = self.footprint_recipe(x_m, z_m)[0];
        if !(n > self.threshold) {
            return 0.0;
        }
        if self.gated {
            self.remap.density(n, self.threshold, self.frame.of(q / self.cell_m))
        } else {
            cover_density(n, self.threshold, FOOTPRINT_EDGE)
        }
    }

    /// Its Size, metres: the cell its footprint recipe stands one cloud in.
    pub fn cell_m(&self) -> f32 {
        self.cell_m
    }

    /// The column at a ground point (metres east and south) integrated straight up through the shell in `levels`
    /// steps, with detail: its opacity, and the lowest and highest heights with density (infinite where none).
    #[cfg(test)]
    pub(crate) fn column(&self, x_m: f32, z_m: f32, levels: usize) -> (f32, f32, f32) {
        let dz = self.thickness / levels as f32;
        let (mut tau, mut low, mut high) = (0.0f32, f32::INFINITY, f32::NEG_INFINITY);
        for level in 0..levels {
            let p = vec3f(x_m, self.base_m + (level as f32 + 0.5) * dz, z_m);
            let c = self.cheap(p, 0.0);
            let full = if c.view > 0.0 { self.full(c.view, c.skin, c.q, p.y, c.h01, 1.0) } else { 0.0 };
            if full > 0.0 {
                tau += full * self.sigma * dz;
                low = low.min(p.y);
                high = high.max(p.y);
            }
        }
        (1.0 - (-tau).exp(), low, high)
    }
}

// --- Nimbostratus --- (look lane ns): its arm of the shape (cloud_volume_nimbostratus.rs) on the Cumulus machinery
/// The Nimbostratus' recipe's id in its light grids' key.
const NS_KEY_PART: f32 = 23.0;

impl VolumeShape {
    /// A Nimbostratus' density field (design §2.2's Nimbostratus row, cloud_volume_nimbostratus.rs): its footprint grid
    /// of masses and Unevenness' thick and thin parts, its shell from the scud's bottom to the deck's highest top, the
    /// Cover solve (design §3) over its deck, and its light grids' key. None at no cover or no density. The tables a
    /// Cumulus reads (tops, rise, levels) are flat: the GPU's upload carries them, its Nimbostratus arm reads none.
    fn build_nimbostratus(spec: &VolumeSpec, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<Option<VolumeShape>, Cancelled> {
        let b = spec.layer.nimbostratus_params();
        let c = spec.cover;
        if !(c > 0.0) || !(b.density > 0.0) {
            return Ok(None);
        }
        let cell_m = b.size_km * 1000.0;
        let stream = cloud_layer_seed(spec.map_seed, spec.layer.seed, spec.genus);
        let streams = Streams::of(stream);
        let drift_cells = spec.layer.drift_km.unwrap_or(0.0) * 1000.0 / cell_m;
        let frame = WindFrame::new(spec.downwind, drift_cells);
        let noise = noise_for(spec.map_seed, cancelled, run)?;
        let billow = noise.billow(cancelled, run)?;
        let mut ns = NsShape::of(&b, c, spec.downwind, &noise.shape, &noise.detail, &billow);
        let (base_m, top_m) = ns.shell_m();
        let thickness = top_m - base_m;
        let bounds = move |_: [f32; 2], _: [f32; 2]| (base_m, top_m);
        let margin = SkipMargin { across_m: 1.0, below_m: 0.0, above_m: 0.0 };
        let grid_key = footprint_grid_key(&[nimbostratus::NS_RECIPE, b.size_km, spec.downwind.x, spec.downwind.y, drift_cells, base_m, top_m], stream, GRID_SIZE, GRID_SPAN_M);
        let at_m = |x: f32, z: f32| nimbostratus::recipe_m(&frame, &streams, cell_m, x, z);
        let grid = shared_footprint_grid(grid_key, &|| FootprintGrid::build(GRID_SIZE, GRID_SPAN_M, &at_m, &bounds, margin, cancelled, run))?;
        let place = |k: u32| (hash_u32(stream ^ k.wrapping_mul(0x68e3_1da4)) >> 8) as f32 * (1.0 / 16_777_216.0);
        let size_km = b.size_km;
        let sigma = nimbostratus::sigma(&b);
        let steps = spec.march_steps.clamp(STEPS_LEAST, STEPS_MOST);
        let bits = f32::from_bits;
        let rows = [ns.base_m, ns.top_m, ns.ragged, ns.unevenness, ns.scud_m, ns.scud, ns.rain, ns.darkness];
        let mut fields = vec![nimbostratus::NS_RECIPE, b.density, sigma, bits(spec.map_seed), bits(grid_key as u32), bits((grid_key >> 32) as u32)];
        fields.extend_from_slice(&rows);
        let key = footprint_grid_key(&fields, stream, TOP_COLS, NS_KEY_PART);
        let set = quantile_set(fingerprint(spec.genus, nimbostratus::NS_FOOTPRINT_PART, &[]));
        // the strongest masses' noise (its table, which the Cover solve reads too): a mass's strength is shaped in its unit
        let measured = Streams::of(MEASURE_SEED);
        let n = |_: u32, x: f32, y: f32| nimbostratus::recipe_at(vec2f(x, y), &measured)[0];
        ns.q_hi = set.table(1, &n).at(nimbostratus::NS_HI_QUANTILE);
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
            tops: vec![1.0; TOP_COLS],
            rise: vec![1.0; RISE_ROWS],
            rise_threshold: 0.0,
            levels: vec![0.0; LEVEL_ROWS + 1],
            shape_scale: 1.0 / (SHAPE_TILE_M * VOLUME_SHAPE_TILE * size_km),
            detail_scale: 1.0 / (DETAIL_TILE_M * size_km),
            detail_texel_m: DETAIL_TILE_M * size_km / noise.detail.size() as f32,
            detail_mean: noise.detail.mean(),
            noise,
            billow,
            offset: vec3f(place(1), place(2), place(3)),
            sigma,
            carve: 1.0,
            billow_cut: 0.0,
            span: 1.0,
            erosion: 0.0,
            lean: Vec2f::default(),
            steps,
            lattice_m: LATTICE_MOST_M,
            cover: c,
            stream,
            key,
            genus: spec.genus,
            stratus: None,
            sc: None,
            ns: Some(ns),
        };
        shape.solve_share(c, &set, &n);
        shape.key = footprint_grid_key(&[bits(key as u32), bits((key >> 32) as u32), shape.threshold, shape.share, shape.gated as u32 as f32], stream, TOP_COLS, NS_KEY_PART);
        Ok(Some(shape))
    }

    /// The Nimbostratus' cheap read (its `cheap`): the deck (`NsShape::deck`) over the footprint, at the footprint grid's
    /// level of `footprint_m`: the mass's strength (its footprint noise over the threshold in the unit `NsShape::span`)
    /// less what its height and lumps use up, over its rim from its outline (and the tail gate under the lowest Covers);
    /// no detail erodes it (its skin is 0), the sun's field is the view's, and its envelope is where it has density (the
    /// fallback's walk finds the deck on its lattice). The CPU marches the deck alone: the scud and the rain are the GPU
    /// arm's.
    #[inline]
    fn ns_cheap(&self, ns: &NsShape, p: Vec3f, footprint_m: f32) -> Cheap {
        let h01 = (p.y - self.base_m) / self.thickness;
        let q = vec2f(p.x, p.z);
        let none = Cheap { h01, q, ..Cheap::default() };
        if !(h01 > 0.0 && h01 < 1.0) || !(p.x.abs() < self.grid.half_m() && p.z.abs() < self.grid.half_m()) {
            return none;
        }
        let [n, column, ..] = self.grid.sample(p.x, p.z, footprint_m);
        // the lumps only dent: no mass stands where its footprint noise does not
        if !(n > self.threshold) {
            return none;
        }
        // (a mass is never stronger than the strongest masses: past them it stands no taller)
        let strength = ((n - self.threshold) / ns.span(self.threshold)).min(1.0);
        // (the remap's density of noise 1 over a threshold of 0 is its tail gate alone)
        let gate = if self.gated { self.remap.density(1.0, 0.0, self.frame.of(q / self.cell_m)) } else { 1.0 };
        let view = ns.deck(p, column, strength, &self.noise.shape) * gate;
        if !(view > 0.0) {
            return none;
        }
        let f = smoothstep(0.0, ns.edge, strength) * gate;
        Cheap { view, h01, q, f, skin: 0.0, sun: view, envelope: true, up: None }
    }
}

/// The angle a texel of a bake `width` texels wide spans as the density's law reads it (the detail's fade, the
/// footprint grid's level, the probe's footprint, the skip's level): never more than a released texel's
/// (LAW_WIDTH_LEAST), so held marches the released clouds.
pub(crate) fn law_angle(width: usize) -> f32 {
    std::f32::consts::TAU / width.max(LAW_WIDTH_LEAST) as f32
}

/// The extinction through the step from sample `a` to the next one, `b`: the mean of the two where `b` follows `a` in
/// one run (the trapezoid: a soft edge rises steeply over a step), else `a`'s own.
#[inline]
fn through(a: &Sample, b: &Sample) -> f32 {
    if b.follows {
        0.5 * (a.sigma + b.sigma)
    } else {
        a.sigma
    }
}

/// The length of the step from sample `a` to the next one, `b`: the gap between them where `b` follows `a` in one run
/// (a run's last step ends on its exit, design §12.1 item 3), else `a`'s own.
#[inline]
fn step_length(a: &Sample, b: &Sample) -> f32 {
    if b.follows {
        b.gap
    } else {
        a.ds
    }
}

/// A lit sample waiting in `CloudVolume::march` for the next one: its sun's depth and the powder's, whether its sun
/// was the hybrid's, and its sun's rate from the sample before it (0 for a run's first).
#[derive(Clone, Copy)]
struct Waiting {
    s: Sample,
    tau: f32,
    local: f32,
    live: bool,
    back: f32,
}

/// A volume layer, ready to march (design §2, §12): its density field and its light's grids.
pub struct CloudVolume {
    shape: VolumeShape,
    sun: SunDepthGrid,
    up: Arc<TauUpGrid>,
    lining: f32,
    /// The share of the sun the ground under it receives: the mean of exp(-tau toward the sun) from SUN_SHARE_POINTS^2
    /// points over its base within COVER_REACH_M of the viewer, through its sun-depth grid (the light pass's sunlit
    /// ground, `sun_share`).
    sun_share: f32,
}

/// The ground's sunlit share is read at this many points along each side of the square round the viewer.
const SUN_SHARE_POINTS: usize = 32;
/// A volume's shadow on a lower layer is read from its sun grid within this share of the grids' reach (its coarse
/// level's last cells fade).
const SHADOW_REACH_SHARE: f32 = 0.95;

impl CloudVolume {
    /// The volume of `spec`, None when `VolumeShape::build` gives none; with its sun-depth grid (both levels, or while
    /// `held` the coarse one, and the fine one a released bake left) and its tau_up grid, looked up or filled (design
    /// §12.1 item 5, §12.6 items 5-7). Rows are spread by `run`, `cancelled` polled.
    pub fn build(spec: &VolumeSpec, held: bool, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<Option<CloudVolume>, Cancelled> {
        let Some(shape) = VolumeShape::build(spec, cancelled, run)? else {
            return Ok(None);
        };
        let grid_spec = shape.sun_grid_spec(spec.sun_dir);
        let extinction = |p: Vec3f| shape.extinction(p);
        let clear = |a: Vec3f, b: Vec3f| shape.span_is_clear(a, b);
        let sun = SunDepthGrid::shared(&grid_spec, shape.key, shape.cover, shape.stream, &extinction, Some(&clear), held, cancelled, run)?;
        let up = TauUpGrid::shared(shape.base_m, shape.top_m, COARSE_RADIUS_M, shape.key, shape.cover, shape.stream, &extinction, Some(&clear), cancelled, run)?;
        let lining = spec.layer.lining.unwrap_or(spec.genus.default_lining());
        let n = SUN_SHARE_POINTS;
        let mut lit = 0.0f32;
        for j in 0..n {
            for i in 0..n {
                let at = |k: usize| ((k as f32 + 0.5) / n as f32 * 2.0 - 1.0) * COVER_REACH_M;
                lit += (-sun.grid_tau(vec3f(at(i), shape.base_m - 1.0, at(j)), 0.0)).exp();
            }
        }
        let sun_share = lit / (n * n) as f32;
        Ok(Some(CloudVolume { shape, sun, up, lining, sun_share }))
    }

    /// The share of the sun the ground under it receives (its mean sun transmittance over its base round the viewer):
    /// the light pass's sunlit ground under a marched volume (design §2.3's bounce; a closed deck's base is lit by the
    /// dome's bounce alone).
    pub fn sun_share(&self) -> f32 {
        self.sun_share
    }

    /// Its shadow on a lower flat layer (design §2.3's Shade below, `LitEntry::plane_cover`): at a point `xz_m` (metres
    /// east and south) of its base, what it takes of the sun there, 1 - exp(-its sun-depth grid's depth). Past the
    /// reach of its footprint and its sun grid, the far deck's: a Nimbostratus' Cover, nothing for the others. One grid
    /// read; it never allocates.
    pub fn shadow_cover(&self, xz_m: Vec2f) -> f32 {
        let reach = SHADOW_REACH_SHARE * self.shape.grid.half_m().min(COARSE_RADIUS_M);
        if !(xz_m.x.abs() < reach && xz_m.y.abs() < reach) {
            // --- Nimbostratus --- (look lane ns): its deck carries on past the grids
            return match &self.shape.ns {
                Some(ns) => nimbostratus::far_cover(self.shape.cover, (ns.top_m - ns.base_m) / self.sun.light_dir().y.max(0.05) / self.shape.cell_m),
                None => 0.0,
            };
        }
        1.0 - (-self.sun.grid_tau(vec3f(xz_m.x, self.shape.base_m - 1.0, xz_m.y), 0.0)).exp()
    }

    /// The lit march along `d` at a bake `width` texels wide, cut at the heights `cuts` (ascending, inside the shell:
    /// the flat entries in it) into `out`'s records, one a segment front to back (design §12.1 item 9); a shorter `out`
    /// keeps its first cuts. Each record is begun at `VolumeMarch::CLEAR`, the stop is on the whole ray, and the record
    /// the ray stopped in is closed. Nothing at and below the horizon. Returns where its steps went.
    pub fn march(&self, d: Vec3f, width: usize, cuts: &[f32], out: &mut [VolumeMarch]) -> WalkCounts {
        for m in out.iter_mut() {
            *m = VolumeMarch::CLEAR;
        }
        let length = d.length();
        if out.is_empty() || !(length > 0.0) {
            return WalkCounts::default();
        }
        let d = d / length;
        if !(d.y > 0.0) {
            return WalkCounts::default();
        }
        let cuts = &cuts[..cuts.len().min(out.len() - 1)];
        let l = self.sun.light_dir();
        // A deck's light is the slab family's (its base lit by the sun that diffuses through it, design §2.3), through the
        // column over each point (a broken deck: `RayLight::diffuse_through_column`); a heap's the heap family's.
        let light = match self.shape.sc {
            Some(_) => RayLight { diffuse_through_column: true, ..RayLight::new(d.dot(l), l.y, self.lining, LightFamily::Slab) },
            None => RayLight::new(d.dot(l), l.y, self.lining, LightFamily::Heap),
        }
        .seen_from(d.y);
        let shape = &self.shape;
        let extinction = |p: Vec3f| shape.extinction(p);
        let (mut seg, mut front) = (0usize, 1.0f32);
        let mut next_cut = cuts.first().map_or(f32::INFINITY, |&h| shell_distance(d.y, h));
        // The sun's depth at a sample (design §12.6 item 4): the hybrid until the ray is 90 % opaque, the grid alone
        // past it.
        let sun_at = |s: &Sample, live: bool| {
            if live {
                let got = self.sun.sun_tau(s.p, s.here, &extinction);
                (got.total(), got.local)
            } else {
                (self.sun.grid_tau(s.p, s.here), s.here * POWDER_REACH_M)
            }
        };
        // A sample's step from it to the next (the sample is the step's start), its extinction `sigma` through the
        // step and the sun's depth along it at `rate`; true where the ray stops in it. A backward rate extrapolates, and
        // where the depth falls fast toward the sun it would carry the step's end below 0 and `step` would light it by
        // exp(+tau) (a 1580x speck under a 9 degree sun): the end is held at 0 before the middle is formed (design
        // §12.1 item 6).
        let take = |m: &mut VolumeMarch, front: f32, w: &Waiting, sigma: f32, ds: f32, rate: f32| {
            let rate = if w.tau + rate * ds < 0.0 { -w.tau / ds } else { rate };
            // A type's own dome where its density gives one (a Stratocumulus'), else the tau_up grid's.
            let up = w.s.up.unwrap_or_else(|| self.up.tau_up(w.s.p));
            step(m, &light, sigma, ds, w.tau + 0.5 * rate * ds, rate, up, w.local, w.s.h01, w.s.t, front);
            if m.stops(front) {
                m.close();
                return true;
            }
            false
        };
        // Each lit step waits for the next sample, so the sun's rate along the view is the forward difference to it:
        // the entry sample of a run (the sunlit skin) gets one too (design §12.6 item 3). A run's last sample takes the
        // backward difference, a lone one none; its step ends on the run's exit, which has no extinction, so it reads
        // no sun and takes no step of its own.
        let mut waiting: Option<Waiting> = None;
        let mut stopped = false;
        let counts = shape.walk(d, width, true, &mut |s| {
            let (live, tau, local, back) = match waiting {
                Some(w) => {
                    let (sigma, ds) = (through(&w.s, s), step_length(&w.s, s));
                    // The ray's transmittance once the waiting step is taken, as `step` leaves it.
                    let after = front * (out[seg].transmittance * (-(sigma * ds)).exp());
                    if after <= MIN_TRANSMITTANCE {
                        stopped = take(&mut out[seg], front, &w, sigma, ds, w.back);
                        return false;
                    }
                    let live = after >= SUN_GRID_PAST;
                    if !(s.sigma > 0.0) {
                        take(&mut out[seg], front, &w, sigma, ds, w.back);
                        (live, 0.0, 0.0, 0.0)
                    } else {
                        let (tau, local) = sun_at(s, live);
                        let forward = if s.follows && live == w.live { Some((tau - w.tau) / s.gap) } else { None };
                        if w.s.sigma > 0.0 {
                            take(&mut out[seg], front, &w, sigma, ds, forward.unwrap_or(w.back));
                        }
                        (live, tau, local, forward.unwrap_or(0.0))
                    }
                }
                None => {
                    let live = front * out[seg].transmittance >= SUN_GRID_PAST;
                    let (tau, local) = sun_at(s, live);
                    (live, tau, local, 0.0)
                }
            };
            while s.t >= next_cut {
                front *= out[seg].transmittance;
                seg += 1;
                next_cut = cuts.get(seg).map_or(f32::INFINITY, |&h| shell_distance(d.y, h));
            }
            waiting = Some(Waiting { s: *s, tau, local, live, back });
            true
        });
        if let (false, Some(w)) = (stopped, waiting.filter(|w| w.s.sigma > 0.0)) {
            take(&mut out[seg], front, &w, w.s.sigma, w.s.ds, w.back);
        }
        counts
    }

    /// What it hides along `d` (any length), unfaded, without light: the march's opacity at the released width
    /// (design §4.2: the key's cone and the facts read it).
    pub fn cover_toward(&self, d: Vec3f) -> f32 {
        let length = d.length();
        if !(length > 0.0) {
            return 0.0;
        }
        self.shape.opacity(d / length, COVER_WIDTH, true)
    }

    /// The light pass's stand-in along `d` (design §4.3): the unlit opacity through the tau_up grid's columns. A look
    /// crosses the shell on a slant through STAND_IN_POINTS stretches; each adds the depth its column loses over the
    /// stretch's rise, over the rise's share of the stretch's length. Its tau is that depth.
    pub fn stand_in(&self, d: Vec3f) -> CoverSample {
        let length = d.length();
        if !(length > 0.0) {
            return CoverSample::default();
        }
        let d = d / length;
        if !(d.y > 0.0) {
            return CoverSample::default();
        }
        let (base, top) = self.shape.shell_m();
        let horizontal = (1.0 - d.y * d.y).max(0.0).sqrt();
        let range = RANGE_M / horizontal.max(1.0e-4);
        // --- Nimbostratus --- (look lane ns): past the march's range its deck carries on to the horizon (its GPU arm's
        // far deck), so a look whose deck base lies past the range sees the far deck's cover
        if let Some(ns) = &self.shape.ns {
            let t_base = shell_distance(d.y, ns.base_m);
            if t_base > range {
                return nimbostratus::far_look(self.shape.cover, (shell_distance(d.y, ns.top_m) - t_base) / self.shape.cell_m);
            }
        }
        let near = shell_distance(d.y, base);
        let far = shell_distance(d.y, top).min(range);
        if !(far > near) {
            return CoverSample::default();
        }
        let dt = (far - near) / STAND_IN_POINTS as f32;
        let mut depth = 0.0f32;
        for k in 0..STAND_IN_POINTS {
            let (t0, t1) = (near + dt * k as f32, near + dt * (k + 1) as f32);
            let (h0, h1) = (height_at(t0, d.y), height_at(t1, d.y));
            let mid = 0.5 * (t0 + t1);
            let (x, z) = (d.x * mid, d.z * mid);
            let lost = (self.up.tau_up(vec3f(x, h0, z)) - self.up.tau_up(vec3f(x, h1, z))).max(0.0);
            if lost > 0.0 && h1 > h0 {
                depth += lost * dt / (h1 - h0);
            }
        }
        CoverSample { cover: 1.0 - (-depth).exp(), tau: depth }
    }

    /// Its density field.
    pub fn shape(&self) -> &VolumeShape {
        &self.shape
    }

    /// Its sun-depth grid (its light direction is the march's: `SunDepthGrid::light_dir`).
    pub fn sun_grid(&self) -> &SunDepthGrid {
        &self.sun
    }

    /// Its tau_up grid.
    pub fn tau_up_grid(&self) -> &TauUpGrid {
        &self.up
    }

    /// Its light record for bakes `width` texels wide, its march cut at `cuts` (design §12.1 item 9), shared
    /// process-wide by the key of everything its march reads: a map rebuilt for a drag of a light row (Silver lining,
    /// Brightness, Sky light, the sun's colour, the haze) finds the record its last bake filled and marches nothing. The
    /// record starts empty and its rows are marched as bakes first read them. Its key is the light grids' (every field
    /// the extinction reads) with the detail's erosion (Puffiness), the sun's way, the grid levels, the step budget, the
    /// width and the cuts.
    pub fn light_record(&self, width: usize, cuts: &[f32]) -> Arc<LightRecord> {
        let (shape, cover, stream) = self.shape.light_key();
        let l = self.sun.light_dir();
        let bits = f32::from_bits;
        let mut fields = vec![bits(shape as u32), bits((shape >> 32) as u32), cover, self.shape.erosion, l.x, l.y, l.z, bits(self.sun.has_fine() as u32), bits(self.shape.steps)];
        fields.extend_from_slice(cuts);
        let key = footprint_grid_key(&fields, stream, width, RECORD_KEY_PART);
        let built = shared_by_key(&RECORDS, key, &|| Ok(LightRecord::new(width, cuts)), "light record", &|record, _| format!("{} x {} kept ({} segments)", record.width, record.rows.len(), record.segments));
        built.unwrap_or_else(|_| Arc::new(LightRecord::new(width, cuts)))
    }

    /// The key of its GPU record for bakes `width` texels wide, its march cut at `cuts`, its lobes mixed at `lining`, in
    /// the shader's `mode`, its texels' directions moved by `jitter` texels (an antialiasing bake's, design §12.1 item
    /// 10; 0 for the texel centres) (design §12.3, §12.11 item 4; task-13 study §3.7): the light record's key with those.
    pub fn gpu_key(&self, width: usize, cuts: &[f32], lining: f32, mode: crate::cloud_volume_gpu::GpuMode, jitter: [f32; 2]) -> u64 {
        let (shape, cover, stream) = self.shape.light_key();
        let l = self.sun.light_dir();
        let bits = f32::from_bits;
        let mut fields = vec![bits(shape as u32), bits((shape >> 32) as u32), cover, self.shape.erosion, l.x, l.y, l.z, bits(self.sun.has_fine() as u32), bits(self.shape.steps), lining, bits(mode as u32), jitter[0], jitter[1]];
        fields.extend_from_slice(cuts);
        footprint_grid_key(&fields, stream, width, GPU_KEY_PART)
    }

    /// What the GPU uploads to march it at bakes `width` texels wide, cut at `cuts` (ascending, at most
    /// `cloud_volume_gpu::PASS_SEGMENTS - 1`), its lobes mixed at `lining`, in `mode`, its texels' directions moved by
    /// `jitter` texels (an antialiasing bake's, design §12.1 item 10) (design §12.3; task-13 study §3.3): its parts
    /// (`cloud_volume_gpu::PartKind`: the footprint grid with its mips and max pyramid, the billow and detail tiles (and
    /// the shape tile in the export mode), the tables with the blue noise, the near bits and tau_up (the CPU mode's),
    /// the sun-depth grid), each built only where `resident` says the
    /// GPU does not hold its key (design §12.11 item 4.6: the preview's asks re-upload only what changed), and every
    /// number its march reads besides as its own uniforms, each module's own (the shader's functions mirror
    /// `VolumeShape::cheap`, `full`, `walk`, `CloudVolume::march`, `SunDepthGrid::sun_tau` and
    /// `cloud_volume_light::step`); its record is made apart (`CloudAsk::make_records`). Err where a module wrote a buffer
    /// into the volume's own uniforms (task-14 review N6). Built on the caller's thread: an export's or a preview's job,
    /// never the UI thread.
    pub fn gpu_upload(&self, width: usize, cuts: &[f32], lining: f32, mode: crate::cloud_volume_gpu::GpuMode, jitter: [f32; 2], resident: &dyn Fn(u64) -> bool) -> Result<crate::cloud_volume_gpu::VolumeUpload, String> {
        use crate::cloud_volume_gpu::{index, key_of, part_key, GpuPart, PartKind, UploadBuilder, PASS_SEGMENTS};
        // The shader's literal loops (entry and exit halvings, the skip's levels, the billows' two levels).
        const _: () = assert!(ENTRY_HALVINGS == 4 && SKIP_LEVELS == 5 && LEVELS == 3);
        let s = &self.shape;
        // A part the GPU holds goes as its key; another is built here. Each part's buffers are its own textures, its
        // offsets from 0 (exact floats under 2^24).
        let part = |kind: PartKind, key: u64, build: &dyn Fn(&mut UploadBuilder)| {
            let key = part_key(kind, key);
            if resident(key) {
                return GpuPart { kind, key, data: None };
            }
            let mut b = UploadBuilder::default();
            build(&mut b);
            b.part(kind, key)
        };
        let grid = part(PartKind::Grid, s.grid_key, &|b| {
            let (levels, bounds) = s.grid.gpu_layout(b);
            b.uniform("fgrid", [s.grid.half_m(), s.grid.levels() as f32, s.grid.first_bounds_level() as f32, levels]);
            b.uniform("fgrid2", [bounds, s.grid.cell_m(0), 0.0, 0.0]);
        });
        // The modes read different parts (look-b review M3): the CPU mode's march reads the near bits and tau_up, the
        // export mode's neither (2.8 MB and 0.45 s of serial work at 8192, 8.9 MB) but the shape tile, its patches
        // (Perlin-Worley, the Horizon look; look lane b; 33.5 MB), which the CPU mode never reads. The shape tile goes
        // in the Tiles part, whose key holds the mode: a CPU mode's Tiles part (without it) never serves an export-mode
        // ask, whose shader would read the billow tile as its shape tile.
        let cpu = mode == crate::cloud_volume_gpu::GpuMode::Cpu;
        let shape_edge = if cpu { 0 } else { s.noise.shape.size() };
        let tiles = part(PartKind::Tiles, key_of(&[s.noise.seed() as u64, s.billow.size() as u64, s.noise.detail.size() as u64, mode as u64, shape_edge as u64]), &|b| {
            let billow = b.vectors(s.billow.gpu_texels());
            let detail = b.vectors(s.noise.detail.gpu_texels());
            b.uniform("tiles", [index(billow), s.billow.size() as f32, index(detail), s.noise.detail.size() as f32]);
            if !cpu {
                let shape_tile = b.vectors(s.noise.shape.gpu_texels());
                b.uniform("tiles2", [index(shape_tile), shape_edge as f32, 0.0, 0.0]);
            }
        });
        // The tables are some 5 000 floats: keyed by their content.
        let ranks = crate::cloud_volume::blue_ranks();
        let content = [&s.tops[..], &s.rise[..], &s.levels[..]].iter().flat_map(|t| std::iter::once(t.len() as u64).chain(t.iter().map(|v| v.to_bits() as u64))).chain(ranks.iter().map(|&r| r as u64)).collect::<Vec<u64>>();
        let tables = part(PartKind::Tables, key_of(&content), &|b| {
            let tops = b.scalars(s.tops.iter().copied());
            let rise = b.scalars(s.rise.iter().copied());
            let level_rows = b.scalars(s.levels.iter().copied());
            let ranks = b.scalars(ranks.iter().map(|&r| r as f32));
            b.uniform("tables", [index(tops), index(rise), index(level_rows), index(ranks)]);
        });
        let near = cpu.then(|| {
            part(PartKind::Near, key_of(&[s.base_m.to_bits() as u64, s.top_m.to_bits() as u64, s.thickness.to_bits() as u64, width as u64, jitter[0].to_bits() as u64, jitter[1].to_bits() as u64]), &|b| {
                b.scalars(s.inside_at_near_bits(width, jitter));
            })
        });
        // (the export mode's Nimbostratus reads tau_up too: the dome through its deck)
        let up = (cpu || s.ns.is_some()).then(|| part(PartKind::TauUp, self.up.gpu_key(), &|b| self.up.gpu_layout(b)));
        let sun = part(PartKind::Sun, self.sun.gpu_key(), &|b| self.sun.gpu_layout(b));
        let mut out = UploadBuilder::default();
        s.remap.gpu_layout(&mut out);
        crate::volumetric::gpu_layout(&mut out);
        crate::cloud_volume_light::gpu_layout(&mut out);
        let [down_x, down_z, drift] = s.frame.gpu();
        out.uniform("shell", [s.base_m, s.top_m, s.thickness, s.sigma]);
        out.uniform("cell", [s.cell_m, s.threshold, s.rise_threshold, s.gated as u32 as f32]);
        out.uniform("wind", [down_x, down_z, drift, 0.0]);
        out.uniform("lean", [s.lean.x, s.lean.y, s.carve, s.billow_cut]);
        out.uniform("noise", [s.shape_scale, s.detail_scale, s.detail_texel_m, s.detail_mean]);
        out.uniform("place", [s.offset.x, s.offset.y, s.offset.z, s.erosion]);
        // (the near bits' offset is 0: they are their own part)
        out.uniform("shape2", [s.span, s.steps as f32, 0.0, 0.0]);
        out.uniform("k_shape1", [DOME_EDGE, DOME_POWER, DEPTH_ACROSS, ACROSS_OWN]);
        out.uniform("k_shape2", [ACROSS_LEAST, CUT_OUTLINE, FILL_FROM, CORE_FROM]);
        out.uniform("k_shape3", [BASE_FILL, MIST_SHARE, MIST_RAMP, ERODE_SPAN]);
        out.uniform("k_shape4", [MIST_FADE.0, MIST_FADE.1, ERODE_DEPTH.0, ERODE_DEPTH.1]);
        out.uniform("k_shape5", [ERODE_KEEP, RISE_DOME_LEAST, FOOTPRINT_EDGE, TOP_COLS as f32]);
        out.uniform("k_level", [LEVEL_ROWS as f32, LEVEL_BLEND.0, LEVEL_BLEND.1, RISE_ROWS as f32]);
        out.uniform("k_place", [LEVEL_PLACE[0], LEVEL_PLACE[1], LEVEL_PLACE[2], DOWN_OWN]);
        out.uniform("k_walk", [PROBE_PATHS, EMPTY_TO_AIR as f32, LATTICE_MOST_M, SKIN_STEP]);
        out.uniform("k_walk2", [SUN_GRID_PAST, SKIP_LEVELS as f32, LAW_WIDTH_LEAST as f32, 0.0]);
        // A missing cut is never reached (its distance is infinite).
        let cut = |k: usize| cuts.get(k).copied().unwrap_or(f32::INFINITY).min(1.0e30);
        debug_assert!(cuts.len() < PASS_SEGMENTS);
        out.uniform("cuts", [cut(0), cut(1), cut(2), cuts.len().min(PASS_SEGMENTS - 1) as f32]);
        out.uniform("light", [lining, mode as u32 as f32, width as f32, 0.0]);
        // The type's own numbers (the upload names the type, whose export shader reads them: design §12.13).
        if let Some(st) = &s.stratus {
            st.gpu_layout(&mut out, s.threshold);
        }
        if let VolumeRecipe::Altocumulus(recipe) = &s.recipe {
            crate::cloud_volume_altocumulus::gpu_layout(&mut out, recipe, &s.streams, s.share);
        }
        if let Some(sc) = &s.sc {
            sc.gpu_layout(s, self.sun.light_dir().y, &mut out);
        }
        if let Some(ns) = &s.ns {
            ns.gpu_layout(&mut out, s.cover, s.threshold);
        }
        // (the texel's direction, moved off its centre for an antialiasing bake)
        out.uniform("jitter", [jitter[0], jitter[1], 0.0, 0.0]);
        let key = self.gpu_key(width, cuts, lining, mode, jitter);
        let segments = cuts.len() + 1;
        let uniforms = out.into_uniforms()?;
        let parts = [Some(grid), Some(tiles), Some(tables), near, up, Some(sun)].into_iter().flatten().collect();
        Ok(crate::cloud_volume_gpu::VolumeUpload { genus: s.genus, key, width, segments, parts, uniforms, record: None })
    }

    /// The building blocks of its march at the point `p` read at a texel footprint of `footprint_m` (task-13 study §5
    /// step 3, the GPU's probe): [the cheap read's view, sun, skin (-1 outside the envelope) and footprint; the grid's
    /// four channels and its strength; tau_up, the extinction, the full density at detail 1 and at the footprint's
    /// detail; the sun grid's far read (in air, all, cloud weight, spread; spread -1 where it keeps no nodes); the
    /// hybrid's depth and local depth, and the grid's alone, at the point's own extinction; the billow and detail
    /// tiles at the point in kilometres (as tiles) and the blue noise at texel (|p.x|, |p.z|)]. Test support for the GPU
    /// crate.
    #[doc(hidden)]
    pub fn gpu_probe(&self, p: Vec3f, footprint_m: f32) -> [f32; 24] {
        let s = &self.shape;
        let c = s.cheap(p, footprint_m);
        let node = s.grid.sample(p.x, p.z, footprint_m);
        let here = s.extinction(p);
        let full = |detail: f32| if c.view > 0.0 { s.full(c.view, c.skin, c.q, p.y, c.h01, detail) } else { 0.0 };
        let fade = 1.0 - smoothstep(0.5 * s.detail_texel_m, s.detail_texel_m, footprint_m);
        let far = self.sun.far_read(p);
        let hybrid = self.sun.sun_tau(p, here, &|q| s.extinction(q));
        let tile = |t: &Tile3| t.sample(p.x * 1.0e-3, p.y * 1.0e-3, p.z * 1.0e-3);
        let jitter = blue_jitter(p.x.abs() as u32, p.z.abs() as u32);
        [
            c.view,
            c.sun,
            if c.envelope { c.skin } else { -1.0 },
            c.f,
            node[0],
            node[1],
            node[2],
            node[3],
            node[4],
            self.up.tau_up(p),
            here,
            full(1.0),
            full(fade),
            far[0],
            far[1],
            far[2],
            far[3],
            hybrid.total(),
            hybrid.local,
            self.sun.grid_tau(p, here),
            tile(&s.billow),
            tile(&s.noise.detail),
            jitter,
            0.0,
        ]
    }
}

/// The GPU record's part in its key (the footprint grid's key function, its span field).
const GPU_KEY_PART: f32 = 13.0;

/// The light records kept, oldest first (`CloudVolume::light_record`).
static RECORDS: GridCache<LightRecord> = Mutex::new(Vec::new());
/// The light record's part in its key (the footprint grid's key function, its span field).
const RECORD_KEY_PART: f32 = 9.0;

/// A volume's light record at one bake width (design §12.1 item 9): for each texel of the upper half of a map
/// `width` x `width / 2` in the map's own frame, the march of its centre's direction, one `VolumeMarch` per segment of
/// the march cut at `cuts`. Its rows are marched the first time a bake reads them, by whichever thread reads first,
/// each once and by the same function, so a bake that reads it equals one that marches every texel itself (the
/// texel's centre) bit for bit. 36 bytes a texel and segment: 2.4 MB at 512 wide, 9.4 MB at 1024.
pub struct LightRecord {
    width: usize,
    height: usize,
    segments: usize,
    cuts: Vec<f32>,
    rows: Vec<OnceLock<Vec<VolumeMarch>>>,
}

impl LightRecord {
    fn new(width: usize, cuts: &[f32]) -> LightRecord {
        let width = width.max(2);
        let height = (width / 2).max(1);
        // The rows over the horizon (v < 0.5); none lower holds a volume.
        let rows = height.div_ceil(2);
        LightRecord { width, height, segments: cuts.len() + 1, cuts: cuts.to_vec(), rows: (0..rows).map(|_| OnceLock::new()).collect() }
    }

    /// The records of the texel holding the unit direction `d` (map frame) into `out` (its first segments, up to its
    /// length), marching `volume` over the texel's row first if no bake has: false, with `out` untouched, where `d`
    /// lies in no recorded texel (at or below the horizon, not a direction).
    pub fn read(&self, volume: &CloudVolume, d: Vec3f, out: &mut [VolumeMarch]) -> bool {
        if !(d.y > 0.0) || !d.is_finite() {
            return false;
        }
        let uv = ibl::dir_to_equirect_uv(arr(d));
        let x = ((uv[0] * self.width as f32) as i64).rem_euclid(self.width as i64) as usize;
        let y = (uv[1] * self.height as f32).max(0.0) as usize;
        let Some(row) = self.rows.get(y) else {
            return false;
        };
        let row = row.get_or_init(|| {
            let v = (y as f32 + 0.5) / self.height as f32;
            let mut marches = vec![VolumeMarch::CLEAR; self.segments];
            let mut row = Vec::with_capacity(self.width * self.segments);
            for x in 0..self.width {
                let centre = vec(ibl::equirect_uv_to_dir([(x as f32 + 0.5) / self.width as f32, v]));
                volume.march(centre, self.width, &self.cuts, &mut marches);
                row.extend_from_slice(&marches);
            }
            row
        });
        let n = out.len().min(self.segments);
        out[..n].copy_from_slice(&row[x * self.segments..x * self.segments + n]);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_classic::downwind;
    use crate::cloud_volume_light::{shade, ShadeInputs};
    use crate::dir_from_az_el;
    use crate::noise::hash01;
    use crate::volumetric::FORWARD_SHARE;

    /// Runs every index on four scoped threads, interleaved, as the bake pool's fan_out would.
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

    /// A Cumulus layer at its defaults, `cover` and `seed` given.
    fn cumulus(cover: f32, seed: u32) -> CloudLayerParams {
        let mut layer = CloudLayerParams::new(CloudGenus::Cumulus, &[], 1);
        layer.cover = Some(cover);
        layer.seed = seed;
        layer
    }

    /// Its spec on map seed 1, the wind from 30 degrees, the sun `elevation` up at azimuth 200, 48 steps.
    fn spec(layer: &CloudLayerParams, cover: f32, elevation: f32) -> VolumeSpec<'_> {
        VolumeSpec { layer, genus: CloudGenus::Cumulus, cover, map_seed: 1, downwind: downwind(30.0), sun_dir: dir_from_az_el(200.0, elevation), march_steps: 48 }
    }

    /// `n` directions spread evenly over the solid angle above `from_deg`, jittered.
    fn cap(n: usize, from_deg: f32, salt: u32) -> Vec<Vec3f> {
        let low = from_deg.to_radians().sin();
        (0..n as i32)
            .map(|k| {
                let y = low + (1.0 - low) * hash01(k, 1, 0, salt);
                let a = std::f32::consts::TAU * hash01(k, 2, 0, salt);
                let r = (1.0 - y * y).max(0.0).sqrt();
                vec3f(r * a.sin(), y, -r * a.cos())
            })
            .collect()
    }

    /// Design §3: the share of the sky above 20 degrees a volume hides (opacity over 0.5 at the released width) is its
    /// Cover within 0.05, at Cover 0.2, 0.5 and 0.8, the mean of 16 seeds (one seed's sky above 20 degrees holds only
    /// some 15 clouds), and a Stratus' the mean of 32. With it, design §2.4's proportions: at the defaults a cloud (a
    /// patch of columns over 0.5 opaque seen from above) is 1.2 to 2 times as wide as it is tall, the median over the
    /// clouds of a 20 km square.
    #[test]
    fn a_volume_cover_is_its_share_of_sky() {
        let dirs = cap(4096, 20.0, 0x5ca1_ab1e);
        for cover in [0.2f32, 0.5, 0.8] {
            let mut shares = Vec::new();
            for seed in 11u32..27 {
                let layer = cumulus(cover, seed);
                let shape = VolumeShape::build(&spec(&layer, cover, 30.0), &|| false, &four_threads).unwrap().expect("a Cumulus volume");
                let hidden = dirs.iter().filter(|&&d| shape.opacity(d, 1024, true) > 0.5).count();
                shares.push(hidden as f32 / dirs.len() as f32);
                let (share, k) = shape.cover_share();
                println!("cover {cover}, seed {seed}: hides {:.3} of the sky above 20 degrees (footprint share {share:.3}, k {k:.3})", shares.last().unwrap());
            }
            let mean = shares.iter().sum::<f32>() / shares.len() as f32;
            println!("cover {cover}: {mean:.3} on average");
            assert!((mean - cover).abs() <= 0.05, "Cover {cover}: the volume hides {mean} of the sky above 20 degrees ({shares:?})");
        }
        // A Stratus (the volumes design §2.2) hides its Cover too: the mean of 32 seeds, as a 400 m layer's sky above 20
        // degrees holds only a few of its patches.
        for cover in [0.2f32, 0.5, 0.8] {
            let mut shares = Vec::new();
            for seed in 11u32..43 {
                let mut layer = CloudLayerParams::new(CloudGenus::Stratus, &[], 1);
                layer.cover = Some(cover);
                layer.seed = seed;
                let spec = VolumeSpec { genus: CloudGenus::Stratus, ..spec(&layer, cover, 30.0) };
                let shape = VolumeShape::build(&spec, &|| false, &four_threads).unwrap().expect("a Stratus volume");
                let hidden = dirs.iter().filter(|&&d| shape.opacity(d, 1024, true) > 0.5).count();
                shares.push(hidden as f32 / dirs.len() as f32);
            }
            let mean = shares.iter().sum::<f32>() / shares.len() as f32;
            println!("Stratus cover {cover}: {mean:.3} on average");
            assert!((mean - cover).abs() <= 0.05, "Cover {cover}: a Stratus hides {mean} of the sky above 20 degrees ({shares:?})");
        }
        // A Stratocumulus (plan: its cover-share case): its share of the sky above 20 degrees is its Cover within 0.05 too,
        // the mean of 8 seeds (a deck's sky holds many cells, fewer seeds average it).
        for cover in [0.2f32, 0.5, 0.8] {
            let mut shares = Vec::new();
            for seed in 11u32..19 {
                let mut layer = CloudLayerParams::new(CloudGenus::Stratocumulus, &[], 1);
                layer.cover = Some(cover);
                layer.seed = seed;
                let sc = VolumeSpec { genus: CloudGenus::Stratocumulus, ..spec(&layer, cover, 30.0) };
                let shape = VolumeShape::build(&sc, &|| false, &four_threads).unwrap().expect("a Stratocumulus volume");
                shares.push(dirs.iter().filter(|&&d| shape.opacity(d, 1024, true) > 0.5).count() as f32 / dirs.len() as f32);
            }
            let mean = shares.iter().sum::<f32>() / shares.len() as f32;
            println!("Stratocumulus cover {cover}: {mean:.3} on average ({shares:?})");
            assert!((mean - cover).abs() <= 0.05, "Stratocumulus Cover {cover}: the volume hides {mean} of the sky above 20 degrees ({shares:?})");
        }
        // Proportions at the defaults: columns 50 m apart over 20 km, each integrated straight up; the patches over 0.5
        // opaque, their width the diameter of a disc of their area, their height from their lowest to their highest
        // point with density.
        let layer = CloudLayerParams::new(CloudGenus::Cumulus, &[], 1);
        let cover = CloudGenus::Cumulus.default_cover();
        let shape = VolumeShape::build(&spec(&layer, cover, 30.0), &|| false, &four_threads).unwrap().unwrap();
        let (side, spacing, levels) = (400usize, 50.0f32, 64usize);
        let (base, top) = shape.shell_m();
        let dz = (top - base) / levels as f32;
        let columns: Vec<(f32, f32, f32)> = (0..side * side)
            .map(|i| {
                let (x, z) = ((i % side) as f32 * spacing - 10_000.0, (i / side) as f32 * spacing - 10_000.0);
                let (mut tau, mut low, mut high) = (0.0f32, f32::INFINITY, f32::NEG_INFINITY);
                for level in 0..levels {
                    let p = vec3f(x, base + (level as f32 + 0.5) * dz, z);
                    let c = shape.cheap(p, 0.0);
                    let full = if c.view > 0.0 { shape.full(c.view, c.skin, c.q, p.y, c.h01, 1.0) } else { 0.0 };
                    if full > 0.0 {
                        tau += full * shape.sigma * dz;
                        low = low.min(p.y);
                        high = high.max(p.y);
                    }
                }
                (1.0 - (-tau).exp(), low, high)
            })
            .collect();
        let mut seen = vec![false; side * side];
        let mut ratios = Vec::new();
        for first in 0..side * side {
            if seen[first] || columns[first].0 <= 0.5 {
                continue;
            }
            let (mut stack, mut area, mut low, mut high, mut edge) = (vec![first], 0usize, f32::INFINITY, f32::NEG_INFINITY, false);
            seen[first] = true;
            while let Some(i) = stack.pop() {
                area += 1;
                low = low.min(columns[i].1);
                high = high.max(columns[i].2);
                let (x, z) = (i % side, i / side);
                edge |= x == 0 || z == 0 || x == side - 1 || z == side - 1;
                for (nx, nz) in [(x.wrapping_sub(1), z), (x + 1, z), (x, z.wrapping_sub(1)), (x, z + 1)] {
                    if nx < side && nz < side && !seen[nz * side + nx] && columns[nz * side + nx].0 > 0.5 {
                        seen[nz * side + nx] = true;
                        stack.push(nz * side + nx);
                    }
                }
            }
            // A cloud whole inside the square and more than a few columns across.
            if !edge && area >= 40 {
                let width = 2.0 * (area as f32 * spacing * spacing / std::f32::consts::PI).sqrt();
                ratios.push(width / (high - low + dz));
            }
        }
        ratios.sort_by(f32::total_cmp);
        let median = ratios[ratios.len() / 2];
        println!("proportions at the defaults: {} clouds, width over height {:.2} (median), {:.2} to {:.2} (quartiles)", ratios.len(), median, ratios[ratios.len() / 4], ratios[3 * ratios.len() / 4]);
        assert!(ratios.len() >= 10, "{} clouds", ratios.len());
        assert!((1.2..=2.0).contains(&median), "a Cumulus at its defaults is {median} times as wide as tall");
    }

    /// The light the light pass gives `layer` under the sun `elevation` up at azimuth 200 (its sun, dome and ground; the
    /// sky behind is not read: every cloud here is nearer than the haze's 8 km), as shading inputs at the layer's
    /// Silver lining.
    fn light_pass(layer: &CloudLayerParams, elevation: f32) -> ShadeInputs {
        let mut p = crate::HdriParams::default();
        p.sky.sun.mode = "manual".to_string();
        p.sky.sun.elevation_deg = elevation;
        p.sky.sun.azimuth_deg = 200.0;
        p.sky.cloud_stack = Some(crate::CloudStackParams { layers: vec![layer.clone()], ..crate::CloudStackParams::default() });
        let light = crate::Env::new(&p).cloud_stack.expect("a Cumulus draws").lights()[0];
        ShadeInputs { lining: FORWARD_SHARE, sun_base: light.sun, sun_top: light.sun, brightness: 1.0, sky_light: 1.0, ambient: light.ambient, side: light.ambient, ground: light.ground, ground_sun: light.ground_sun, sky: light.ambient, visibility_m: 60_000.0 }
    }

    /// The radiance (red, of `inputs`' sun, dome and ground) a released march along `d` shows of an opaque cloud: None
    /// where it is not over 0.95 opaque.
    fn opaque_radiance(volume: &CloudVolume, d: Vec3f, inputs: &ShadeInputs) -> Option<f32> {
        let mut out = [VolumeMarch::CLEAR];
        volume.march(d, 1024, &[], &mut out);
        let alpha = out[0].alpha();
        (alpha > 0.95).then(|| shade(&out[0], inputs).x / alpha)
    }

    /// Design §8 and §12.8 item 4 (a Cumulus' base is darker than its top, as much as the reference's): the sun 30
    /// degrees up, the clouds seen from the ground in columns of views 6 to 45 degrees up, every 2 degrees of azimuth,
    /// 0.05 degrees a row; each crossing of a cloud at least a degree tall and centred 8 to 40 degrees up is one
    /// cloud seen from below (its lowest 15 %) and from the side above (its upper 25 %), three rows in from either
    /// edge, as refs.md measured kloofendal_48d (base-to-top 0.70). The median over the crossings of their base's median
    /// luminance over their top's, in the light the light pass gives the layer, of three seeds, is held within 30 % of
    /// the reference's: 0.49 to 0.91.
    #[test]
    fn cumulus_bases_are_darker_than_tops() {
        const ROW_DEG: f32 = 0.05;
        let ratios = Mutex::new(Vec::new());
        for seed in [21u32, 22, 23] {
            let layer = cumulus(0.35, seed);
            let inputs = light_pass(&layer, 30.0);
            let volume = CloudVolume::build(&spec(&layer, 0.35, 30.0), false, &|| false, &four_threads).unwrap().unwrap();
            let rows = ((45.0 - 6.0) / ROW_DEG) as usize;
            four_threads(180, &|column| {
                let az = 2.0 * column as f32;
                // Each row's luminance where the cloud stands over half opaque, None where it does not.
                let lum: Vec<Option<f32>> = (0..rows)
                    .map(|row| {
                        let d = dir_from_az_el(az, 6.0 + ROW_DEG * (row as f32 + 0.5));
                        let mut out = [VolumeMarch::CLEAR];
                        volume.march(d, 1024, &[], &mut out);
                        let alpha = out[0].alpha();
                        (alpha > 0.5).then(|| {
                            let c = shade(&out[0], &inputs) / alpha;
                            0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z
                        })
                    })
                    .collect();
                let median = |mut v: Vec<f32>| {
                    v.sort_by(f32::total_cmp);
                    v[v.len() / 2]
                };
                let mut row = 0;
                while row < rows {
                    if lum[row].is_none() {
                        row += 1;
                        continue;
                    }
                    let start = row;
                    while row < rows && lum[row].is_some() {
                        row += 1;
                    }
                    let n = row - start;
                    let centre = 6.0 + ROW_DEG * (start as f32 + 0.5 * n as f32);
                    // Whole crossings only: not cut by the band's ends.
                    if n < 20 || start == 0 || row == rows || !(8.0..=40.0).contains(&centre) {
                        continue;
                    }
                    let at = |a: usize, b: usize| median((a..b.max(a + 1)).map(|r| lum[r].unwrap()).collect());
                    let base = at(start + 3, start + (0.15 * n as f32) as usize);
                    let top = at(row - (0.25 * n as f32) as usize, row - 3);
                    ratios.lock().unwrap().push(base / top);
                }
            });
        }
        let mut ratios = ratios.into_inner().unwrap();
        ratios.sort_by(f32::total_cmp);
        let n = ratios.len();
        assert!(n >= 50, "{n} crossings");
        let ratio = ratios[n / 2];
        println!("base-to-top over {n} crossings: {ratio:.3} (median; quartiles {:.3} to {:.3}); the reference's 0.70", ratios[n / 4], ratios[3 * n / 4]);
        assert!((0.49..=0.91).contains(&ratio), "a Cumulus' base shows {ratio} of its top (the reference's 0.70, held within 30 %)");
        // A Stratocumulus' row (plan): its underside is darker than its edges lit through its gaps. The sun 30 degrees
        // up, the deck (Cover 0.7) seen from the ground 50 to 85 degrees up, every 2 degrees of azimuth, 0.05 degrees a
        // row; each crossing of the deck at least 2 degrees long between two gaps: its edges (the two rows beside either
        // gap) and its underside (its middle half). The median over the crossings of their underside's median luminance
        // over their edges', of four seeds, is under 0.7.
        let ratios = Mutex::new(Vec::new());
        for seed in [51u32, 52, 53, 54] {
            let mut layer = CloudLayerParams::new(CloudGenus::Stratocumulus, &[], 1);
            layer.cover = Some(0.7);
            layer.seed = seed;
            let inputs = light_pass(&layer, 30.0);
            let sc = VolumeSpec { genus: CloudGenus::Stratocumulus, ..spec(&layer, 0.7, 30.0) };
            let volume = CloudVolume::build(&sc, false, &|| false, &four_threads).unwrap().unwrap();
            let rows = ((85.0 - 50.0) / ROW_DEG) as usize;
            four_threads(180, &|column| {
                let az = 2.0 * column as f32;
                let lum: Vec<Option<f32>> = (0..rows)
                    .map(|row| {
                        let d = dir_from_az_el(az, 50.0 + ROW_DEG * (row as f32 + 0.5));
                        let mut out = [VolumeMarch::CLEAR];
                        volume.march(d, 1024, &[], &mut out);
                        let alpha = out[0].alpha();
                        (alpha > 0.5).then(|| {
                            let c = shade(&out[0], &inputs) / alpha;
                            0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z
                        })
                    })
                    .collect();
                let median = |mut v: Vec<f32>| {
                    v.sort_by(f32::total_cmp);
                    v[v.len() / 2]
                };
                let mut row = 0;
                while row < rows {
                    if lum[row].is_none() {
                        row += 1;
                        continue;
                    }
                    let start = row;
                    while row < rows && lum[row].is_some() {
                        row += 1;
                    }
                    let n = row - start;
                    // Between two gaps: not cut by the band's ends.
                    if n < 40 || start == 0 || row == rows {
                        continue;
                    }
                    let edges = median(vec![lum[start].unwrap(), lum[start + 1].unwrap(), lum[row - 2].unwrap(), lum[row - 1].unwrap()]);
                    let under = median((start + n / 4..row - n / 4).map(|r| lum[r].unwrap()).collect());
                    ratios.lock().unwrap().push(under / edges);
                }
            });
        }
        let mut ratios = ratios.into_inner().unwrap();
        ratios.sort_by(f32::total_cmp);
        let n = ratios.len();
        let ratio = ratios[n / 2];
        println!("Stratocumulus underside-to-edge over {n} crossings: {ratio:.3} (median; quartiles {:.3} to {:.3})", ratios[n / 4], ratios[3 * n / 4]);
        assert!(n >= 30, "{n} crossings of the deck");
        assert!(ratio < 0.7, "a Stratocumulus' underside shows {ratio} of its gap-lit edges");
    }

    /// Design §8 (the sunlit side of a Cumulus is brighter than its shaded side): the sun 30 degrees up, clouds seen
    /// across the sun's way (azimuths 90 degrees off it, 10 to 30 degrees up), each crossing of a cloud along a row of
    /// the view split into its half toward the sun's azimuth and its half away: the sun's light on the near halves is at
    /// least 1.3 times the far halves'. Printed beside it: the cloud texels' light looking toward the sun and away
    /// from it, 20 to 30 degrees up.
    #[test]
    fn the_sunlit_side_is_brighter() {
        let all = light_pass(&cumulus(0.35, 31), 30.0);
        let sun_only = ShadeInputs { ambient: Vec3f::default(), ground: Vec3f::default(), ground_sun: Vec3f::default(), ..all };
        let (mut near, mut far) = (0.0f64, 0.0f64);
        let (mut toward, mut away) = (Vec::new(), Vec::new());
        for seed in [31u32, 32] {
            let layer = cumulus(0.35, seed);
            let volume = CloudVolume::build(&spec(&layer, 0.35, 30.0), false, &|| false, &four_threads).unwrap().unwrap();
            for (across, sign) in [(110.0f32, 1.0f32), (290.0, -1.0)] {
                for row in 0..21 {
                    let el = 10.0 + row as f32;
                    // Light along the row, azimuth rising; the sun's side is at rising azimuth for the view at 110
                    // degrees (the sun at 200) and at falling azimuth for the one at 290.
                    let lit: Vec<Option<f32>> = (0..800).map(|i| opaque_radiance(&volume, dir_from_az_el(across - 40.0 + i as f32 * 0.1, el), &sun_only)).collect();
                    let mut i = 0;
                    while i < lit.len() {
                        if lit[i].is_none() {
                            i += 1;
                            continue;
                        }
                        let from = i;
                        while i < lit.len() && lit[i].is_some() {
                            i += 1;
                        }
                        let run: Vec<f32> = lit[from..i].iter().map(|v| v.unwrap()).collect();
                        if run.len() >= 12 && from > 0 && i < lit.len() {
                            let half = run.len() / 2;
                            let (low, high): (f64, f64) = (run[..half].iter().map(|&v| v as f64).sum(), run[run.len() - half..].iter().map(|&v| v as f64).sum());
                            let (sunward, shaded) = if sign > 0.0 { (high, low) } else { (low, high) };
                            near += sunward;
                            far += shaded;
                        }
                    }
                }
            }
            for k in 0..800 {
                let el = 20.0 + 10.0 * hash01(k, 5, 0, seed);
                let spread = 60.0 * (hash01(k, 6, 0, seed) - 0.5);
                toward.extend(opaque_radiance(&volume, dir_from_az_el(200.0 + spread, el), &all));
                away.extend(opaque_radiance(&volume, dir_from_az_el(20.0 + spread, el), &all));
            }
        }
        let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len().max(1) as f32;
        println!("the sun on the sunward halves against the far halves: {:.2}; looking toward the sun {:.3} ({} texels), away {:.3} ({} texels): {:.2}", near / far, mean(&toward), toward.len(), mean(&away), away.len(), mean(&toward) / mean(&away));
        assert!(far > 0.0 && near >= 1.3 * far, "the sunward halves get {near}, the far halves {far}");
    }

    /// Design §12.4: the march that crosses empty pyramid cells in one move sees what the plain march sees: its opacity
    /// is the plain one's within 1e-4 on 1000 rays (at 512, 1024 and 4096 wide), with the same samples in cloud, and it
    /// reads fewer lattice points. Grown with: `cover_toward` is the lit march's alpha bit for bit (300 rays).
    #[test]
    fn the_skipped_march_equals_the_plain_march() {
        let layer = cumulus(0.35, 41);
        let shape = VolumeShape::build(&spec(&layer, 0.35, 30.0), &|| false, &four_threads).unwrap().unwrap();
        let (mut worst, mut equal) = (0.0f32, 0usize);
        let (mut skipped, mut plain) = (WalkCounts::default(), WalkCounts::default());
        for k in 0..1000 {
            let d = dir_from_az_el(360.0 * hash01(k, 7, 0, 3), 1.0 + 89.0 * hash01(k, 8, 0, 3));
            let width = [512usize, 1024, 4096][k as usize % 3];
            let (a, b) = (shape.opacity(d, width, true), shape.opacity(d, width, false));
            worst = worst.max((a - b).abs());
            equal += (a.to_bits() == b.to_bits()) as usize;
            let (s, p) = (shape.walk(d, width, true, &mut |_| true), shape.walk(d, width, false, &mut |_| true));
            assert_eq!((s.cloud, s.empty), (p.cloud, p.empty), "the same samples in cloud along {d:?}");
            for (sum, one) in [(&mut skipped, s), (&mut plain, p)] {
                sum.air += one.air;
                sum.skips += one.skips;
                sum.cloud += one.cloud;
            }
        }
        println!(
            "skipped against plain: {equal} of 1000 rays bit for bit, off by {worst:e} at most; lattice points read {} against {} ({} moves over empty cells); {} samples in cloud",
            skipped.air, plain.air, skipped.skips, skipped.cloud
        );
        assert!(worst <= 1.0e-4, "the skipped march's opacity is off the plain one's by {worst}");
        assert!(skipped.air < plain.air, "the skip read {} lattice points, the plain walk {}", skipped.air, plain.air);
        // The lit march cut at two heights into three records composites to the uncut one (the compositor's rule:
        // each record seen through the ones in front of it): the transmittances multiply to the uncut one's and the
        // light sums to it within 1e-5, and the records past the one the ray stopped in stay clear. The uncut march's
        // alpha is `cover_toward`'s bit for bit.
        let volume = CloudVolume::build(&spec(&layer, 0.35, 30.0), false, &|| false, &four_threads).unwrap().unwrap();
        let (base, top) = shape.shell_m();
        let cuts = [base + 0.3 * (top - base), base + 0.6 * (top - base)];
        let (mut cut_worst, mut stopped_early) = (0.0f32, 0usize);
        for k in 0..300 {
            let d = dir_from_az_el(360.0 * hash01(k, 9, 0, 3), 3.0 + 87.0 * hash01(k, 10, 0, 3));
            let (mut whole, mut parts) = ([VolumeMarch::CLEAR], [VolumeMarch::CLEAR; 3]);
            volume.march(d, 512, &[], &mut whole);
            volume.march(d, 512, &cuts, &mut parts);
            // The unlit march (`cover_toward`, at the released width) takes the lit march's steps, its runs' exits
            // included: its opacity is the lit march's alpha bit for bit.
            let mut released = [VolumeMarch::CLEAR];
            volume.march(d, COVER_WIDTH, &[], &mut released);
            assert_eq!(volume.cover_toward(d).to_bits(), released[0].alpha().to_bits(), "cover_toward against the lit march along {d:?}");
            let (mut front, mut light) = (1.0f32, 0.0f32);
            let mut ended = false;
            for m in &parts {
                if ended {
                    assert_eq!(*m, VolumeMarch::CLEAR, "a record past the stop along {d:?}");
                }
                light += front * (m.forward_low + m.forward_high + m.back_low + m.back_high + m.ambient + m.open);
                front *= m.transmittance;
                ended |= m.transmittance == 0.0;
                stopped_early += (ended && m.transmittance == 0.0 && !std::ptr::eq(m, &parts[2])) as usize;
            }
            let w = &whole[0];
            let want = w.forward_low + w.forward_high + w.back_low + w.back_high + w.ambient + w.open;
            cut_worst = cut_worst.max((front - w.transmittance).abs()).max((light - want).abs() / want.max(1.0));
        }
        println!("cut in three: off the uncut march by {cut_worst:e} at most; {stopped_early} rays stopped before the last record");
        assert!(cut_worst <= 1.0e-5, "a cut march is off the uncut one by {cut_worst}");
        // A Puffiness drag moves the solved threshold the extinction reads, so the light grids' key moves with it.
        let mut puffier = layer.clone();
        let block = layer.cumulus_params();
        puffier.cumulus = Some(crate::CumulusParams { puffiness: if block.puffiness > 0.5 { block.puffiness - 0.4 } else { block.puffiness + 0.4 }, ..block });
        let other = VolumeShape::build(&spec(&puffier, 0.35, 30.0), &|| false, &four_threads).unwrap().unwrap();
        assert!(other.threshold != shape.threshold && other.light_key().0 != shape.light_key().0, "a Puffiness drag keeps the light grids' key");
    }
}
