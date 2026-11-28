//! A placed cloud volume (design §2.1's placed class, §4.3): a Cumulonimbus standing at (azimuth, distance), marched
//! only where a ray meets its bounds, with no 32 km limit. It does not stand on the field types' grids (the footprint, sun-depth and tau_up grids are built round the viewer out to 34 km,
//! and a storm stands 5 to 120 km out): its shape is closed-form, an envelope of distances in metres covered with the
//! billow tile's cauliflower, and its marches read it directly.
//!
//! - **The envelope** ([`Envelope`]), in the cloud's own frame (along its wind, across it, up): a tower of TURRETS
//!   turrets (the updraft, three masses on its flanks and a flanking line stepping down upwind), each a column wider at
//!   its foot than under its domed cap, bulging with PUFFS puffs, joined smoothly; a base, flat as a whole and ragged at
//!   its edge; an anvil at the top, a plate thick at the tower and thinning downwind to a frayed edge that curls down,
//!   flaring into the tower; an overshooting dome over the updraft; a rain shaft under the base, slanting downwind as it
//!   falls, to the ground. Cover erodes all of it from its edges in, the thin anvil first, down to nothing
//!   (design §4.3).
//! - **The density**: the envelope's distance pushed out (never in) by the cauliflower, big domes of many sizes carrying
//!   small ones on their crowns (lumps in metres after lane b's, their tile scaled to the turret: the updraft's on the
//!   updraft and its masses, half that on the flanking line, and a sixteenth of it where a texel resolves it), then
//!   cumulus cells in its outer 800 m (the billow and shape tiles over a profile, cut softly) and a solid core; on the anvil the shape tile stretched along the wind (fibres fanning out with it)
//!   moves a soft skin and frays its far end into strands, and its underside sags in slow lobes; the rain's curtains are
//!   the detail tile stretched upright. The GPU export's `cb_dens` (makepad-cloud-gpu, export/cumulonimbus.rs) is the
//!   same function; [`PlacedVolume::gpu_upload`] hands it these numbers.
//! - **The CPU march** ([`PlacedVolume::march`]) is the fallback and the preview's: rough, on the same density, lit by
//!   the shared light law (`cloud_volume_light::step`) with short sun and up marches. The export's look is the GPU's.
//!
//! Positions are metres round the viewer: x east, y up, z south, the view ray's height on the curved Earth
//! (`volumetric::height_at`), as the field volumes'.

use makepad_draw::*;
use makepad_render_material::ibl;
use crate::caster::RowRunner;
use crate::cloud_light::{CoverSample, LightFamily};
use crate::cloud_volume::{blue_jitter, footprint_grid_key};
use crate::cloud_volume_cumulus::CUMULUS_SIGMA;
use crate::cloud_volume_gpu::{GpuMode, VolumeUpload};
use crate::cloud_volume_light::{step, RayLight, VolumeMarch, POWDER_REACH_M};
use crate::atmosphere::SunBands;
use crate::rainbow::{Bow, Drops, BOW_PASS};
use crate::noise::hash_u32;
use crate::volumetric::{height_at, noise_for, shell_distance, Cancelled, NoiseVolume, Tile3, MIN_TRANSMITTANCE};
use crate::{arr, cloud_layer_seed, CloudGenus, CloudLayerParams};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

/// The tower's turrets: the updraft (the anvil's foot), three masses on its flanks and a flanking line of three.
pub const TURRETS: usize = 7;
/// The first turrets that make the updraft's mass; the rest are the flanking line.
const MAIN_TURRETS: usize = 4;
/// A Cumulonimbus is this much denser than a Cumulus at Density 1 (design §2.4: infinigen's ranges put it at 1.3x).
const SIGMA_SHARE: f32 = 1.3;
/// A turret is wider at its foot than under its cap: its radius falls by TAPER from its foot to its cap's start (the
/// updraft's by UPDRAFT_TAPER: narrowed as much, it stood as a neck under the anvil, a mushroom), and its cap is a dome
/// CAP_SHARE of its radius high.
const TAPER: f32 = 0.45;
const UPDRAFT_TAPER: f32 = 0.1;
const CAP_SHARE: f32 = 1.0;
/// The turrets bulge: each has puffs at these shares of its height, PUFF_RING of them at each (the updraft a ring of
/// three at three heights, each ring turned on and jittered, so its outline bulges from every side; each flank mass one
/// at two heights, each flanking-line turret one), heights jittered by PUFF_JITTER. A lone puff is a sphere PUFF_SIZE of
/// its turret's radius there and PUFF_OFF of it off its axis, turned its own way; a ring's are RING_SIZE and RING_OFF;
/// sizes jittered by a tenth; joined over PUFF_JOIN of a puff's radius. The first MAIN_PUFFS are the updraft's mass's.
/// (Round 2 carved the envelope into the billow tile's balls: the tile's top 2 % is one flat value, and the balls' tops
/// cut there stood as flat coins, with pits between the balls.)
const PUFF_HEIGHTS: [&[f32]; TURRETS] = [&[0.18, 0.45, 0.72], &[0.3, 0.62], &[0.3, 0.62], &[0.3, 0.62], &[0.45], &[0.45], &[0.45]];
const PUFF_RING: [usize; TURRETS] = [3, 1, 1, 1, 1, 1, 1];
const PUFFS: usize = 18;
const MAIN_PUFFS: usize = 15;
const PUFF_SIZE: f32 = 0.75;
const PUFF_OFF: f32 = 0.45;
const RING_SIZE: f32 = 0.62;
const RING_OFF: f32 = 0.5;
const PUFF_JITTER: f32 = 0.08;
const PUFF_JOIN: f32 = 0.2;
/// A turret's column is COLUMN_SHARE of its radius, so its puffs make its outline from every side (at its full radius
/// the updraft stood as a straight column from the sun's side); the updraft's is UPDRAFT_COLUMN of it (thinner, it
/// stood as a neck under the anvil).
const COLUMN_SHARE: f32 = 0.7;
const UPDRAFT_COLUMN: f32 = 0.85;
/// The cauliflower (round 3, after lane b's lumps in metres): big domes of many sizes, the billow tile's balls (its
/// tile's three sizes riding on each other) blended CAULI_SOFT with the shape tile's soft Perlin-Worley billows, pushing
/// out from CAULI_CAP1 of the blend up, each CAULI_HEIGHT1 of a quarter of its tile high; small domes, the billow tile
/// four times finer pushing from CAULI_CAP2, riding on the big ones' crowns (from CAULI_RIDE of their push up, so the
/// creases between big domes stay smooth), CAULI_HEIGHT2 of a quarter of their tile high. Round 2's lumps (the balls
/// alone, packed from 0.3) were equal caps strewn over a smooth face, bubble wrap; the balls alone from 0.05 were
/// cells of equal domes ringed by craters, the shape tile alone pitted sponge. Its tile is LUMP_TILE x the tower's
/// radius on the updraft and its masses and half that on the flanking line (domes of about 1000, 500 and 250 m
/// carrying domes four times smaller on the default tower), and the view reads a tile a sixteenth of that where a
/// texel resolves it. Outside the surface the domes fade to nothing at twice their height (lane b's rule). The crevices
/// between domes keep CREVICE_SHADE less of the dome's and the diffusion's light (the Cumulus' 0.4 ringed every dome).
const LUMP_TILE: f32 = 1.0;
const CAULI_CAP1: f32 = 0.3;
const CAULI_CAP2: f32 = 0.2;
const CAULI_RIDE: f32 = 0.4;
const CAULI_HEIGHT1: f32 = 0.6;
/// Puffiness scales the big domes' height: CAULI_HEIGHT1 at PUFFINESS_AT (the row's default), so a lower Puffiness is
/// smoother and 1 more heaped.
const PUFFINESS_AT: f32 = 0.7;
const CAULI_HEIGHT2: f32 = 0.3;
const CAULI_SOFT: f32 = 0.5;
const LUMP_FINE: f32 = 0.0625;
const CREVICE_SHADE: f32 = 0.3;
/// On the anvil's plate near the tower (its outflow, before its ice) the domes keep 1 - PLATE_DOMES_OFF of their height:
/// at full height they stood on its flat top as loose popcorn.
const PLATE_DOMES_OFF: f32 = 0.6;
/// Toward the base the domes fade out over BASE_DOMES_M (bulging past the base's footprint, each dome's underside was
/// cut flat at the ragged base's height: terraced shelves seen from below, scallops along the base's edge).
const BASE_DOMES_M: f32 = 500.0;
/// The tower is cumulus cells (round 7, the Cumulus deck's recipe at the storm's scale): the billow tile's balls at the
/// cauliflower's tile blended CELL_MIX with the shape tile's soft billows (CELL_SOFT_TILE of that tile), over a profile
/// falling from CELL_IN_M inside the domed surface to CELL_OUT_M outside it, cut softly at CELL_CUT over CELL_RAMP, and
/// faded in over CELL_BASE_M above the ragged base. Round 5's skin (TOWER_SKIN_M deep over the envelope, eroded by the
/// detail tile's Worley cells) was one opaque wall, so lit from the front every point of it had the same short sun
/// depth: flat white plaster, its pits the only detail (the owner's "grey broccoli", "not like a cloud at all").
/// TOWER_SKIN_M still bounds the empty-space moves. The anvil's skin is ANVIL_SKIN_M.
const TOWER_SKIN_M: f32 = 150.0;
const CELL_SOFT_TILE: f32 = 0.3;
const CELL_IN_M: f32 = 1200.0;
const CELL_OUT_M: f32 = 500.0;
const CELL_MIX: f32 = 0.5;
const CELL_CUT: f32 = 0.15;
const CELL_RAMP: f32 = 0.4;
const CELL_BASE_M: f32 = 120.0;
/// The fluffy outside (round 8; the owner: "too solid", "a wide brick of concrete"): toward the outside the cells fall
/// CELL_FALL under the cut, fray (eaten by up to FRAY of two octaves of the soft tile, FRAY_TILE of the cells' and a third
/// of that) and thin to CELL_THIN of the density (growing as the profile squared), fading to nothing at the band's outer
/// edge. At a quarter of full density 300 m of storm cloud was still opaque (sigma 0.078 a metre), so the edge stayed
/// hard.
const CELL_FALL: f32 = 0.6;
const CELL_THIN: f32 = 0.05;
const FRAY: f32 = 0.7;
const FRAY_TILE: f32 = 0.2;
/// The overshoot is a cauliflower head where it is the nearest body: its domes at HEAD_TILE of the updraft's tile and at
/// full height (the plate's flattening off: it stood as a smooth low bump).
const HEAD_TILE: f32 = 0.5;
/// The anvil's far end breaks into cirrus patches with gaps: the shape tile GAP_TILE_M along and across (in the fan's
/// shares), opening up to GAPS of it, the fibres' texture inside them (combed strands alone read as a comb).
const GAP_TILE_M: [f32; 2] = [4000.0, 1600.0];
const GAPS: f32 = 0.85;
/// The GPU's sun march reads the cauliflower over its first SUN_DOME_STEPS steps (about 850 m toward the sun from mid
/// tower), so the domes shadow each other (over the first five, about 190 m, a front-lit tower was an even wall).
const SUN_DOME_STEPS: f32 = 8.0;
const ANVIL_SKIN_M: f32 = 150.0;
/// The anvil is ice: ANVIL_DENSITY of the tower's extinction over its first ANVIL_DENSE of its reach, falling as
/// exp(-ANVIL_DECAY reach) past it and to ANVIL_THIN of that at its rims, so it grades from the dark thick outflow into
/// translucent ice (round 3's flat 0.15 left it opaque to 0.7 of its reach and translucent a few kilometres on, a step
/// from dark to bright; falling from the tower on, it let the dome's light through onto the tower's shaded side).
const ANVIL_DENSITY: f32 = 0.3;
const ANVIL_DENSE: f32 = 0.3;
const ANVIL_DECAY: f32 = 3.5;
const ANVIL_THIN: f32 = 0.05;
/// The anvil: a plate fanning out downwind from ANVIL_NEAR of the tower's radius to its radius times ANVIL_SPREAD.0 +
/// ANVIL_SPREAD.1 Anvil across, reaching the radius times 1 + ANVIL_REACH Anvil downwind (design §4.3 wrote 1 + 5 Anvil:
/// 16 km on the default tower, a hammer's head, where real anvils spread far wider than their tower) and ANVIL_UPWIND of
/// that upwind, its far end broad and its upwind nose round; thick at the tower and THIN_END of that at its far end;
/// past FRAY_FROM of its reach its edge frays into the shape tile's fibres (FIBRE_TILE_M along, up, across: long cirrus
/// strands fanning out with the plate, and FIBRE_FINE of a second octave FIBRE_TILE2_M), which also move its surface by
/// FIBRE_M, FIBRE_FRAY times more at the far end; over the same stretch it curls down by CURL_SHARE of its thickness.
/// Its thickness falls as (1 - reach)^ANVIL_FALL: a wedge, thick at the tower, a long thin reach (at 1.6 it read as a
/// plank of even thickness at 25-40 km). Its top sags downwind by ANVIL_DROOP of its thickness at the tower times
/// reach^1.5, and the shape tile (ANVIL_TOP_TILE_M along and across, in the fan's shares) moves its top by up to
/// ANVIL_TOP_WAVE of its thickness there: its top line was ruler-straight.
const ANVIL_NEAR: f32 = 0.75;
const ANVIL_REACH: f32 = 7.0;
const ANVIL_SPREAD: (f32, f32) = (0.6, 1.6);
const ANVIL_UPWIND: f32 = 0.25;
const ANVIL_FALL: f32 = 2.4;
const ANVIL_DROOP: f32 = 0.25;
const ANVIL_TOP_WAVE: f32 = 0.25;
const ANVIL_TOP_TILE_M: [f32; 2] = [5000.0, 2000.0];
/// The anvil grows in over ANVIL_LEAST..ANVIL_FULL of the Anvil row: none under ANVIL_LEAST (calvus, the owner's
/// ruling), its thickness and its join to the tower growing to full by ANVIL_FULL. A top with no anvil smooths into a
/// bald mass (calvus): over the tower's upper part (from SMOOTH_FROM of its height) its domes lose up to SMOOTH_TOP of
/// their height, fading out as the row reaches SMOOTH_TO, and faint upright striations (the shape tile
/// STRIATION_TILE_M across and up) move its surface by STRIATION_M.
const ANVIL_LEAST: f32 = 0.05;
const ANVIL_FULL: f32 = 0.2;
const SMOOTH_TO: f32 = 0.25;
const SMOOTH_FROM: f32 = 0.55;
const SMOOTH_TOP: f32 = 0.85;
const STRIATION_M: f32 = 60.0;
const STRIATION_TILE_M: [f32; 2] = [400.0, 3000.0];
const FIBRE_FINE: f32 = 0.35;
const FIBRE_TILE2_M: [f32; 3] = [3000.0, 120.0, 230.0];
/// The anvil's underside sags in slow lobes, the shape tile read UNDER_TILE_M along and across (in the fan's shares),
/// down by up to UNDER_SAG of its thickness there, from the tower on (a smooth plate's underside read as a plank from
/// below and from the sun's side; lifted too, the lobes' troughs carved caves into its thin upwind nose).
const UNDER_TILE_M: [f32; 2] = [6000.0, 2500.0];
const UNDER_SAG: f32 = 0.3;
/// The anvil is the tower's outflow (dense, crisp and lumpy as the tower) over its first ICE_FROM of its reach and ice
/// past ICE_BY (at the ice's density from the tower on it was a translucent saucer on a column).
const ICE_FROM: f32 = 0.05;
const ICE_BY: f32 = 0.6;
const FIBRE_FRAY: f32 = 1.0;
const THIN_END: f32 = 0.08;
const FRAY_FROM: f32 = 0.55;
const CURL_SHARE: f32 = 0.3;
const FIBRE_TILE_M: [f32; 3] = [9000.0, 250.0, 700.0];
const FIBRE_M: f32 = 120.0;
/// The base, flat as a whole, is ragged by the shape tile in two octaves: up to BASE_RAG_M over BASE_RAG_TILE_M a tile
/// (its broad undulation; at 70 m over 1.8 km it drew a ruler-straight base at 25 km, the detail tile's Worley cones drew
/// teeth) and BASE_RAG2 of that over BASE_RAG_TILE2_M, varying with height too (torn bits: one octave hung the base's
/// edge in regular festoons).
const BASE_RAG_M: f32 = 200.0;
const BASE_RAG_TILE_M: f32 = 4000.0;
const BASE_RAG2: f32 = 0.45;
const BASE_RAG_TILE2_M: f32 = 1100.0;
/// The rain: its cores are the detail tile at RAIN_TILE_M metres a tile (along, up, across: kilometre cores, nearly
/// upright, so a ray across the shaft meets a few and the shaft shows streaks; at 500 m they averaged out into a box),
/// streaked by the tile RAIN_STREAK times finer; its density a RAIN_SHARE of the cloud's at Rain shaft 1 (with straight
/// sides and a hard foot that drew an opaque box), soft across its whole radius, which the cores make ragged by up to a
/// quarter more, RAIN_GROUND of it at the ground, falling RAIN_SLANT metres downwind a metre down. Rain reaches the ground
/// at the rate it leaves the base (a 1 mm drop loses a few per cent of itself a kilometre to evaporation): at 0.1 the
/// shaft held a fifth of its rain 300 m up, and a rainbow in it faded out toward its feet, where the look runs longest
/// through sunlit rain and photos show it brightest (the rainbow lane, round 3: the bow over Alexander's band beside it
/// 0.8-1.0 within 8 degrees of the horizon at 0.1, 1.4-2.2 at 1).
const RAIN_TILE_M: [f32; 3] = [2500.0, 20_000.0, 2500.0];
const RAIN_STREAK: f32 = 4.0;
const RAIN_SHARE: f32 = 0.12;
const RAIN_GROUND: f32 = 1.0;
const RAIN_SLANT: f32 = 0.25;
/// The rain is uneven and partly see-through (round 8; the owner: the shaft read as a wide brick of concrete, an even
/// grey column from the base to the ground): it parts into separate shafts (the soft shape tile RAIN_SHAFT_M across,
/// stretched four times upright, cut at RAIN_SHAFT_CUT over 0.2), some of them end above the ground (up to RAIN_END of
/// the base's height, by a second read RAIN_END_TILE times coarser, faded in over RAIN_END_FADE_M), the cores make its
/// sides ragged by up to 0.3 of its radius either way, and it holds RAIN_THIN of round 3's density.
const RAIN_SHAFT_M: f32 = 1500.0;
const RAIN_SHAFT_CUT: f32 = 0.3;
const RAIN_END: f32 = 0.4;
/// Only shafts whose ends read is over RAIN_END_FROM end in the air, as virga here and there (round 8b: from 0.45 most
/// did, and the bow lost its feet, where photos show it brightest; at 0.6 the ends of a near shower lined up over the
/// horizon as a bright band under the curtain).
const RAIN_END_FROM: f32 = 0.8;
const RAIN_END_TILE: f32 = 1.7;
const RAIN_END_FADE_M: f32 = 400.0;
/// Round 3's rain (RAIN_SHARE of the cloud's extinction, about 0.0094 a metre at Rain shaft 1) was some five times
/// heavier rain than real downpours (heavy rain lets one see 1-3 km, about 0.001-0.003 a metre), so a look inside the bow
/// met nothing but sunlit drops and the bow's inside stood 4.6 times Alexander's band, where photos of showers show
/// 1.2-1.9 times (the background and the light scattered between drops fill the band). At RAIN_THIN 0.15 (about 0.0014 a
/// metre) a near shower measures 1.1 and the curtain still reads grey.
const RAIN_THIN: f32 = 0.15;
/// Right under the base the rain is denser, up to 1 + VIRGA times, fading over VIRGA_M down (the base's dark underside
/// ended in a hard line over the paler shaft).
const VIRGA: f32 = 2.0;
const VIRGA_M: f32 = 500.0;
/// The Darkness row darkens the lower tower on the GPU: its sun and open-side light fall by DARK_BASE x Darkness at the
/// base, back to full by DARK_TO of its height (the cloud skill's base darkening: a storm's base is darker than its
/// tower, front-lit too).
const DARK_BASE: f32 = 1.4;
const DARK_TO: f32 = 0.5;
/// On the GPU a storm keeps STORM_DIFFUSION of the law's diffusion (front-lit, the law's diffusion filled the shade under
/// every dome: a plaster wall), and toward the sun a backlit rim: the sun's light scattered forward through the first
/// steps of the sun march only, RIM of the single-scatter term's, dimmed by RIM_SHADOW of the full march's depth, growing
/// with the view-sun cosine from RIM_FROM to RIM_TO (a Cumulus' rim is a thin skin under a texel at 25-40 km, and the
/// law's octaves die in a storm's depth). The CPU fallback keeps the shared law.
const STORM_DIFFUSION: f32 = 0.6;
const RIM: f32 = 0.6;
const RIM_SHADOW: f32 = 0.004;
const RIM_FROM: f32 = 0.2;
const RIM_TO: f32 = 0.9;
/// Under the ragged base's lowest point the rain stands in the storm's shadow: the GPU lights it by the sky beside it at
/// RAIN_OPEN of the open side's share, faded in over 300 m (lit as an open face, the shaft took the horizon's colour;
/// from the nominal base down, the ragged base's own cloud went dark with a hard edge). The CPU fallback keeps the
/// shared law.
const RAIN_OPEN: f32 = 0.35;
/// The flanking line's towers stand LINE_FROM + LINE_STEP j of the tower's radius upwind, LINE_RADIUS - LINE_SHRINK j of
/// it wide at their foot (at 0.45 - 0.07 j, 1.15 + 0.45 j they stood as three candles beside the tower).
const LINE_FROM: f32 = 1.25;
const LINE_STEP: f32 = 0.55;
const LINE_RADIUS: f32 = 0.6;
const LINE_SHRINK: f32 = 0.08;
/// The turrets join over this share of the tower's radius, the updraft's mass and the flanking line over LINE_JOIN of
/// it (at the turrets' join the line stood apart); the tower and the anvil over ANVIL_JOIN of it (the tower's
/// top flares into the anvil's underside; over 0.3 of it the anvil sat on the tower as a cap); the overshoot over
/// OVERSHOOT_JOIN_M.
const TURRET_JOIN: f32 = 0.12;
const LINE_JOIN: f32 = 0.3;
const ANVIL_JOIN: f32 = 0.6;
/// The base's distance counts BASE_SOFT of itself: its skin is softer than the tower's sides (as soft, the ragged base
/// was a hard sheet in terraces from below).
const BASE_SOFT: f32 = 0.35;
const OVERSHOOT_JOIN_M: f32 = 300.0;
/// The overshooting dome over the anvil: OVERSHOOT_M high at Overshoot 1 (at 2500 m no dome showed at 25-40 km),
/// OVERSHOOT_WIDE of the updraft's radius wide.
const OVERSHOOT_M: f32 = 4000.0;
const OVERSHOOT_WIDE: f32 = 0.55;
/// The light law's powder share on a storm (the export's xe.w is the Cumulus' 1.5): a storm 20-40 km out shows no
/// crevice finer than a texel, and the powder only greyed its front-lit faces.
const CB_POWDER: f32 = 0.6;
/// The sun's share on a storm over the export's law's (xf.x): a storm's tower is far deeper than a Cumulus, so more of
/// the sun's light leaves its sunlit faces (set by eye, round 1; a Cycles check is open).
const CB_SUN_SHARE: f32 = 1.4;
/// What the bounds keep round the envelope for rounding, metres.
const BOUNDS_MARGIN_M: f32 = 300.0;
/// Inside the rain shaft a march moves at most this far without a read (the curtains have gaps the cloud's free
/// distance would jump).
const RAIN_STEP_M: f32 = 60.0;
/// The rain's sunlight is marched once a texel of a table and read from it (`RainTable`): up to this wide the table is
/// the bake's own texels, a wider bake (4K, 8K) reads one half as wide, upsampled bilinearly: a quarter of the marches.
const RAIN_FULL_WIDTH: usize = 2048;
/// How many tables (bake width and march segment) a placed volume keeps; past them a look marches the cells it reads
/// itself, unkept: the marches a table would hold, so the map is the same.
const RAIN_TABLES: usize = 16;
/// A table's cell not marched yet (a NaN no march returns).
const RAIN_UNMARCHED: u32 = u32::MAX;
/// A look this close to a table cell's centre, in cells, reads that cell alone: a bake as wide as the table reads
/// exactly the marches of its own texels. The uv's round trip moves a centre by up to about 3.3e-3 of a cell, in the
/// rows by the zenith at 2048 wide (its acos is ill-conditioned there); an antialiasing jitter and a wider bake's
/// half-wide read lie 0.125 of a cell or more off a centre.
const RAIN_SNAP: f32 = 1.0e-2;
/// The rain's sun: the storm's shadow read where the sun's ray from a rain point meets the base (RAIN_SHADE_M over
/// it), lit across RAIN_SHADE_EDGE_M of the envelope's distance there, and RAIN_SHADOW_TAU deep where it is shadowed.
const RAIN_SHADE_M: f32 = 60.0;
const RAIN_SHADE_EDGE_M: f32 = 300.0;
const RAIN_SHADOW_TAU: f32 = 40.0;
/// The storm's shadow on its rain is tabled for the rain's sunlight in cells this wide (`ShadeGrid`): a quarter of the
/// shadow's soft edge, so the table's bilinear reads hold it within a few per cent of the light.
const SHADE_CELL_M: f32 = 0.25 * RAIN_SHADE_EDGE_M;
/// The CPU march: its sun march's steps and growth, its up march's steps from UP_FIRST_M doubling, the longest step in
/// thin cloud (rain), and its steps a mean free path of the body it is in long times this.
const CPU_SUN_STEPS: usize = 6;
const CPU_SUN_GROWTH: f32 = 2.2;
/// The CPU sun march's steps from this one on read the cells' smooth profile (Lod::Soft), as the GPU's far steps (lod 0,
/// past SUN_DOME_STEPS of its fourteen: about a tenth of the way out; the CPU's fourth step starts at 9 % of it).
const SUN_SOFT_FROM: usize = 3;
const CPU_UP_STEPS: usize = 5;
const UP_FIRST_M: f32 = 80.0;
/// A storm's up marches read no finer than UP_SOFT_M a footprint (both marches; the GPU's cb_aa.x): read as fine as
/// the view's, every bulge cast a crisp patch of shade on the dome's light over the tower's shadow side (at 270 m faint
/// patches remained).
const UP_SOFT_M: f32 = 500.0;
/// How many of a ray's first fresh sun marches average two on the GPU (cb_aa.y; the first averages three): at two, the
/// later samples' single jittered marches left a fine grain on the shadow side, and at eight some remained.
const TWO_MARCHES: f32 = 1000.0;
/// The GPU's cost in a close storm's skin (round 6): its dome marches again only UP_AGAIN_M along the ray from the last
/// (cb_ak.y; they read UP_SOFT_M soft, so samples a few metres apart marched the same four marches), and a fresh sun
/// march averages two only while the ray's transmittance is over PAIRS_TO (cb_ak.z; deeper, a sample's light and its
/// grain are dimmed by what is in front of it).
const UP_AGAIN_M: f32 = 120.0;
const PAIRS_TO: f32 = 0.3;
/// The dome's light through the storm over a sample: four up marches tilted UP_TILT (a sine) off the zenith (east, west,
/// south and north), averaged, at UP_DEPTH of their depth (straight up, a bulge's or the anvil's shade fell on the
/// tower's shadow side as crisp patches; two left faint ones).
const UP_TILT: f32 = 0.5;
const UP_DEPTH: f32 = 1.0;
const CPU_STEP_MOST_M: f32 = 300.0;
const CPU_STEP_PATHS: f32 = 0.6;
/// The CPU's marches read the density no finer than a texel this many to a turn (the preview's), as the field volumes'.
const LAW_WIDTH_LEAST: usize = 1024;
/// The GPU record's part in its key.
const GPU_KEY_PART: f32 = 29.0;

/// Smooth minimum (polynomial, Quilez's): `a` and `b` joined over `k` metres.
#[inline]
fn smin(a: f32, b: f32, k: f32) -> f32 {
    let h = (0.5 + 0.5 * (b - a) / k).clamp(0.0, 1.0);
    b + (a - b) * h - k * h * (1.0 - h)
}

pub(crate) use crate::patterns::smooth as smoothstep;

/// What a density read takes: the light marches' far reads (the turrets alone, the tower's cells as their smooth
/// profile: the GPU's lod 0), the CPU's cheap reads (the turrets alone with the cells: the sun march's near steps, the
/// opacity, the bow's column), the light march's near steps (with the cauliflower), the view's (everything).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Lod {
    Soft,
    Far,
    Near,
    View,
}

/// The tower's shapes laid out by field so the CPU can read several distances together.
#[derive(Clone, Copy, Debug, Default)]
struct Shapes {
    turret_x: [f32; TURRETS],
    turret_z: [f32; TURRETS],
    turret_top: [f32; TURRETS],
    /// The column radius at each turret's foot.
    turret_r0: [f32; TURRETS],
    turret_taper: [f32; TURRETS],
    /// Height from the common base to each turret's top.
    turret_span: [f32; TURRETS],
    puff_x: [f32; PUFFS],
    puff_y: [f32; PUFFS],
    puff_z: [f32; PUFFS],
    puff_r: [f32; PUFFS],
    /// Distance over which each puff joins its turret's mass.
    puff_join: [f32; PUFFS],
}

/// The numbers the density reads (both marches, and the GPU's uniforms): the storm's place and frame, its envelope,
/// its noise.
#[derive(Clone, Copy, Debug)]
struct Envelope {
    /// The tower's middle on the ground (x east, z south), metres.
    centre: Vec2f,
    /// Downwind (x, z), a unit.
    wind: Vec2f,
    base: f32,
    top: f32,
    /// The tower's radius (half its width).
    radius: f32,
    /// The tower's lean: metres downwind a metre up.
    lean: f32,
    /// Each turret: (along, across) from the middle, its radius at its foot, its top.
    turrets: [[f32; 4]; TURRETS],
    /// Each puff: (along, up, across) in the leaned frame, its radius.
    puffs: [[f32; 4]; PUFFS],
    shapes: Shapes,
    /// The anvil: its reach downwind, its half width there, its thickness at the tower (0: none), its reach upwind; its
    /// half width at the tower; how far it joins the tower, metres.
    anvil: [f32; 4],
    anvil_near: f32,
    anvil_join: f32,
    /// How bald its top is (1 with no anvil: calvus).
    smooth_top: f32,
    /// How much darker its base is (DARK_BASE x Darkness).
    dark: f32,
    /// The overshooting dome: radius and height (0: none).
    over: [f32; 2],
    /// The rain shaft: radius, strength (0: none), slant, its foot downwind of the middle.
    rain: [f32; 4],
    /// Extinction per metre at density 1.
    sigma: f32,
    /// The updraft's cauliflower tile, metres; the big domes' height (a share of a quarter of their tile: Puffiness); the
    /// domes' highest push, metres.
    lump_tile: f32,
    dome_height: f32,
    lump_most: f32,
    /// Where each noise is read in its tile: the anvil's underside, the cauliflower, the base's raggedness, the anvil's
    /// fibres, the rain.
    place: [Vec3f; 5],
}

/// What the density gives at a point: the density (0..1 of sigma), the height through the storm (0 at its base, 1 at its
/// top), a distance (metres) a march may move without missing cloud (0 near it), whether that distance is the
/// envelope's (safe), not the cauliflower's (which overstates the way to the next lump), and in cloud the density deep
/// in the body it is in (the tower's 1, the anvil's ice, the rain's: the march's step rule); and the rain's density where
/// the rain is the body drawn (under the ragged base, at least as dense as the cloud there; else 0): the sun lights it
/// by the drops' phase (`PlacedVolume::rain_light`), not by the light law's lobes.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Read {
    pub(crate) density: f32,
    pub(crate) h01: f32,
    pub(crate) free_m: f32,
    pub(crate) safe: bool,
    pub(crate) full: f32,
    pub(crate) rain: f32,
}

/// The noise tiles the density reads.
pub(crate) struct Tiles {
    pub(crate) noise: Arc<NoiseVolume>,
    pub(crate) billow: Arc<Tile3>,
}

impl Envelope {
    /// The storm of `spec` (its block, its effective cover, its seed's stream and its wind). Cover scales it by its root
    /// about its base's middle, as a lens' (design §3.1): a small Cover is a small storm that grows with it (eroded from
    /// its edges, rounds 1-3's rule, a small Cover drew nothing).
    fn of(layer: &CloudLayerParams, cover: f32, stream: u32, wind: Vec2f) -> Envelope {
        let b = layer.cumulonimbus_params();
        let scale = cover.clamp(0.0, 1.0).sqrt();
        let base = b.base_m;
        let depth = (b.tops_m - base).max(1.0) * scale;
        let top = base + depth;
        let radius = 500.0 * b.tower_km * scale;
        let a = b.azimuth_deg.to_radians();
        let d = 1000.0 * b.distance_km;
        let unit = |k: u32| (hash_u32(stream ^ k.wrapping_mul(0x9e37_79b9)) >> 8) as f32 * (1.0 / 16_777_216.0);
        let mut turrets = [[0.0f32; 4]; TURRETS];
        // The updraft: a column into the anvil.
        turrets[0] = [0.0, 0.0, 0.8 * radius, top];
        // Three masses on its flanks, domed from under half to most of the way up.
        for k in 1..MAIN_TURRETS {
            let angle = std::f32::consts::TAU * (k as f32 / 3.0 + 0.15 * unit(10 + k as u32));
            let rho = radius * (0.5 + 0.12 * unit(20 + k as u32));
            turrets[k] = [rho * angle.cos(), rho * angle.sin(), radius * (0.5 + 0.1 * unit(30 + k as u32)), base + depth * (0.4 + 0.3 * unit(40 + k as u32))];
        }
        // The flanking line upwind: three towers stepping down away from it.
        for j in 0..TURRETS - MAIN_TURRETS {
            let (k, jf) = (MAIN_TURRETS + j, j as f32);
            turrets[k] = [-radius * (LINE_FROM + LINE_STEP * jf), radius * 0.4 * (unit(50 + k as u32) - 0.5), radius * (LINE_RADIUS - LINE_SHRINK * jf), base + depth * (0.5 - 0.1 * jf) * (0.9 + 0.2 * unit(60 + k as u32))];
        }
        // Each turret's puffs, turned their own ways.
        let mut puffs = [[0.0f32; 4]; PUFFS];
        let mut n = 0;
        for (k, (t, heights)) in turrets.iter().zip(PUFF_HEIGHTS).enumerate() {
            let taper = if k == 0 { UPDRAFT_TAPER } else { TAPER };
            let ring = PUFF_RING[k];
            let turn0 = unit(190 + k as u32);
            for (i, &f0) in heights.iter().enumerate() {
                for j in 0..ring {
                    let m = n as u32;
                    let f = f0 + PUFF_JITTER * (unit(400 + m) - 0.5);
                    let r = t[2] * (1.0 - taper * f);
                    let (size, off, turn) = if ring > 1 {
                        (RING_SIZE, RING_OFF, turn0 + (j as f32 + 0.5 * i as f32) / ring as f32 + 0.15 * (unit(200 + m) - 0.5))
                    } else {
                        (PUFF_SIZE, PUFF_OFF, unit(200 + m))
                    };
                    let turn = std::f32::consts::TAU * turn;
                    let size = size * (0.9 + 0.2 * unit(300 + m));
                    puffs[n] = [t[0] + off * r * turn.cos(), base + f * (t[3] - base), t[1] + off * r * turn.sin(), size * r];
                    n += 1;
                }
            }
        }
        debug_assert_eq!(n, PUFFS);
        let along = radius * (1.0 + ANVIL_REACH * b.anvil);
        let anvil_on = smoothstep(ANVIL_LEAST, ANVIL_FULL, b.anvil);
        let lump_tile = LUMP_TILE * radius;
        let dome_height = CAULI_HEIGHT1 * b.puffiness / PUFFINESS_AT;
        let lump_most = 0.25 * (dome_height + 0.25 * CAULI_HEIGHT2) * lump_tile * (1.0 + LUMP_FINE);
        let place = |k: u32| vec3f(unit(100 + 3 * k), unit(101 + 3 * k), unit(102 + 3 * k));
        let mut shapes = Shapes::default();
        for (i, t) in turrets.iter().enumerate() {
            let (taper, share) = if i == 0 { (UPDRAFT_TAPER, UPDRAFT_COLUMN) } else { (TAPER, COLUMN_SHARE) };
            shapes.turret_x[i] = t[0];
            shapes.turret_z[i] = t[1];
            shapes.turret_top[i] = t[3];
            shapes.turret_r0[i] = share * t[2];
            shapes.turret_taper[i] = taper;
            shapes.turret_span[i] = t[3] - base;
        }
        for (j, s) in puffs.iter().enumerate() {
            shapes.puff_x[j] = s[0];
            shapes.puff_y[j] = s[1];
            shapes.puff_z[j] = s[2];
            shapes.puff_r[j] = s[3];
            shapes.puff_join[j] = PUFF_JOIN * s[3];
        }
        Envelope {
            centre: vec2f(d * a.sin(), -d * a.cos()),
            wind,
            base,
            top,
            radius,
            lean: b.lean_deg.to_radians().tan(),
            turrets,
            puffs,
            shapes,
            anvil: [along, radius * (ANVIL_SPREAD.0 + ANVIL_SPREAD.1 * b.anvil), b.anvil_m * scale * anvil_on, ANVIL_UPWIND * along],
            anvil_near: ANVIL_NEAR * radius,
            anvil_join: ANVIL_JOIN * radius * anvil_on,
            smooth_top: 1.0 - smoothstep(ANVIL_LEAST, SMOOTH_TO, b.anvil),
            dark: DARK_BASE * b.darkness,
            over: [OVERSHOOT_WIDE * turrets[0][2], OVERSHOOT_M * b.overshoot * scale],
            rain: [0.75 * radius * (0.6 + 0.4 * b.rain_shaft), b.rain_shaft, RAIN_SLANT, 0.15 * radius],
            sigma: CUMULUS_SIGMA * SIGMA_SHARE * b.density * (0.75 + 0.5 * b.darkness),
            lump_tile,
            dome_height,
            lump_most,
            place: [place(0), place(1), place(2), place(3), place(4)],
        }
    }

    /// The top of everything it draws (the overshoot over the anvil's top).
    fn ceiling(&self) -> f32 {
        self.top + self.over[1] + BOUNDS_MARGIN_M
    }

    /// How far the noise can push the surface out, metres (and the cells' fluffy band past it).
    fn reach(&self) -> f32 {
        self.lump_most + CELL_OUT_M + FIBRE_M * (1.0 + FIBRE_FRAY) + (UNDER_SAG + ANVIL_TOP_WAVE) * self.anvil[2] + 0.25 * self.anvil_join + STRIATION_M
    }

    /// The two upright cylinders that hold it (x, z, radius): the tower with its rain, from the ground to the ceiling;
    /// the anvil, from under its thickness and its curl to the ceiling.
    fn bounds(&self) -> ([f32; 3], [f32; 3], f32) {
        let reach = self.reach() + BOUNDS_MARGIN_M;
        let shift = self.lean * (self.top - self.base);
        let turrets = self.turrets.iter().map(|t| (t[0] * t[0] + t[1] * t[1]).sqrt() + t[2]).fold(0.0f32, f32::max);
        let puffs = self.puffs.iter().map(|s| (s[0] * s[0] + s[2] * s[2]).sqrt() + s[3]).fold(0.0f32, f32::max);
        let turrets = turrets.max(puffs);
        let rain = self.rain[3].abs() + 1.25 * self.rain[0] + self.rain[2] * self.base;
        let tower_r = turrets.max(rain) + shift.abs() + reach;
        let tower = [self.centre.x, self.centre.y, tower_r];
        // The anvil's plate, from its upwind reach to its downwind one, round its middle.
        let mid = shift + 0.5 * (self.anvil[0] - self.anvil[3]);
        let half = 0.5 * (self.anvil[0] + self.anvil[3]);
        let anvil_r = (half * half + self.anvil[1] * self.anvil[1]).sqrt() + reach;
        let anvil = [self.centre.x + self.wind.x * mid, self.centre.y + self.wind.y * mid, anvil_r];
        (tower, anvil, self.top - self.anvil[2] * (1.0 + CURL_SHARE) - reach)
    }

    /// The local frame of a point: (along the wind, up, across).
    #[inline]
    fn local(&self, p: Vec3f) -> (f32, f32, f32) {
        let (qx, qz) = (p.x - self.centre.x, p.z - self.centre.y);
        (qx * self.wind.x + qz * self.wind.y, p.y, qz * self.wind.x - qx * self.wind.y)
    }

    /// The tower's distance at (leaned along, across, up): (the updraft's mass, the flanking line), each its turrets and
    /// their puffs joined. The GPU's `cb_tower` mirrors it.
    /// The distances are computed independently so the loops can vectorise, then joined in their original order.
    #[inline]
    fn tower(&self, ul: f32, v: f32, y: f32) -> (f32, f32) {
        let s = &self.shapes;
        let mut dt = [0.0f32; TURRETS];
        for i in 0..TURRETS {
            let r = s.turret_r0[i] * (1.0 - s.turret_taper[i] * ((y - self.base) / s.turret_span[i]).clamp(0.0, 1.0));
            let rc = CAP_SHARE * r;
            // These bounds give the same value as clamp without its assertion, so the loop can vectorise.
            let yc = y.max(self.base - 2000.0).min(s.turret_top[i] - rc);
            let (dx, dz, dy) = (ul - s.turret_x[i], v - s.turret_z[i], (y - yc) * (r / rc));
            dt[i] = (dx * dx + dz * dz + dy * dy).sqrt() - r;
        }
        let mut dp = [0.0f32; PUFFS];
        for j in 0..PUFFS {
            let (dx, dy, dz) = (ul - s.puff_x[j], y - s.puff_y[j], v - s.puff_z[j]);
            dp[j] = (dx * dx + dy * dy + dz * dz).sqrt() - s.puff_r[j];
        }

        let ks = TURRET_JOIN * self.radius;
        let mut dm = dt[0];
        for i in 1..MAIN_TURRETS {
            dm = smin(dm, dt[i], ks);
        }
        for j in 0..MAIN_PUFFS {
            dm = smin(dm, dp[j], s.puff_join[j]);
        }
        let mut df = dt[MAIN_TURRETS];
        for i in MAIN_TURRETS + 1..TURRETS {
            df = smin(df, dt[i], ks);
        }
        for j in MAIN_PUFFS..PUFFS {
            df = smin(df, dp[j], s.puff_join[j]);
        }
        (dm, df)
    }

    /// The anvil's distance at (along from the leaned top's axis, across, up), how far downwind the point is (0 at the
    /// tower, 1 at the far end), its thickness and its top there: a plate under the top from `anvil[3]` upwind to `anvil[0]` downwind, its half width
    /// from `anvil_near` to `anvil[1]`, its ends rounded (a superellipse), its thickness `anvil[2]` at the tower falling
    /// fast and then slowly to THIN_END downwind (ANVIL_FALL) and to nothing upwind and at its sides, curling down by
    /// CURL_SHARE of it over its far end. The GPU's `cb_anvil` mirrors it.
    #[inline]
    fn anvil_distance(&self, ut: f32, v: f32, y: f32) -> (f32, f32, f32, f32) {
        let [along, far, thick, up] = self.anvil;
        let s = (ut / along).clamp(0.0, 1.0);
        let e = if ut >= 0.0 { ut / along } else { -ut / up };
        // its far end broad (a superellipse), its upwind nose round (an ellipse: square, it read as a slab's end)
        let ends = if ut >= 0.0 { (1.0 - (e * e) * (e * e)).max(0.0).sqrt() } else { (1.0 - e * e).max(0.0).sqrt() };
        let w = (self.anvil_near + (far - self.anvil_near) * s) * ends;
        let across = (1.0 - (v / w.max(1.0)) * (v / w.max(1.0))).max(0.0).sqrt();
        let fall = if ut >= 0.0 { THIN_END + (1.0 - THIN_END) * (1.0 - s).powf(ANVIL_FALL) } else { 1.0 - e.min(1.0) * e.min(1.0) };
        let top = self.top - ANVIL_DROOP * thick * s.powf(1.5) - CURL_SHARE * thick * smoothstep(FRAY_FROM, 1.0, s);
        let th = thick * fall * across;
        let vertical = (y - top).max(top - th - y);
        let side = (v.abs() - w).max((e - 1.0) * along.min(up));
        (vertical.max(side), s, th, top)
    }

    /// The storm's cauliflower (LUMP_TILE's notes) for tiles of `c1` metres at the storm-frame point (u, y, v) read for
    /// a texel `footprint_m` wide: (how far its domes push the surface out in metres, the crevices' weight). The GPU's
    /// `cb_cauli` mirrors it.
    #[inline]
    fn cauli(&self, tiles: &Tiles, u: f32, y: f32, v: f32, footprint_m: f32, c1: f32) -> (f32, f32) {
        let p = self.place[1];
        let c2 = 0.25 * c1;
        let mut b1 = tiles.billow.sample(u / c1 + p.z, y / c1 + p.x, v / c1 + p.y);
        if CAULI_SOFT > 0.0 {
            b1 += (tiles.noise.shape.sample(u / c1 + p.y, y / c1 + p.z, v / c1 + p.x) - b1) * CAULI_SOFT;
        }
        let b2 = tiles.billow.sample(u / c2 + p.x + 0.41, y / c2 + p.y + 0.13, v / c2 + p.z + 0.77);
        let k1 = smoothstep(CAULI_CAP1, 1.0, b1) * (1.0 - smoothstep(0.02 * c1, 0.06 * c1, footprint_m));
        let k2 = smoothstep(CAULI_CAP2, 1.0, b2) * (1.0 - smoothstep(0.02 * c2, 0.06 * c2, footprint_m)) * smoothstep(CAULI_RIDE, 1.0, k1);
        (0.25 * (self.dome_height * c1 * k1 + CAULI_HEIGHT2 * c2 * k2), 0.7 * k1 + 0.3 * k2)
    }

    /// The cauliflower on the tower, its tile `c1` on the updraft's mass, half that on the flanking line, and both
    /// blended between (`flank`). The GPU's `cb_lumps` mirrors it.
    #[inline]
    fn turret_lumps(&self, tiles: &Tiles, u: f32, y: f32, v: f32, footprint_m: f32, c1: f32, flank: f32) -> (f32, f32) {
        if flank < 0.01 {
            return self.cauli(tiles, u, y, v, footprint_m, c1);
        }
        if flank > 0.99 {
            return self.cauli(tiles, u, y, v, footprint_m, 0.5 * c1);
        }
        let a = self.cauli(tiles, u, y, v, footprint_m, c1);
        let b = self.cauli(tiles, u, y, v, footprint_m, 0.5 * c1);
        (a.0 + (b.0 - a.0) * flank, a.1 + (b.1 - a.1) * flank)
    }

    /// The density at `p` read for a texel `footprint_m` wide at `lod`. The GPU's `cb_dens` mirrors it.
    fn read(&self, tiles: &Tiles, p: Vec3f, footprint_m: f32, lod: Lod) -> Read {
        let (u, y, v) = self.local(p);
        let (base, top) = (self.base, self.top);
        let h01 = ((y - base) / (top - base)).clamp(0.0, 1.0);
        let ul = u - self.lean * (y - base).clamp(0.0, top - base);
        // The tower: the updraft's mass and the flanking line, turrets bulging with their puffs.
        let (dm, df) = self.tower(ul, v, y);
        let dt = smin(dm, df, LINE_JOIN * self.radius);
        // The anvil, flaring into the tower's top; the overshooting dome over the updraft.
        let ut = u - self.lean * (top - base);
        // (no anvil under ANVIL_LEAST: calvus)
        let has_anvil = self.anvil[2] > 0.0;
        let (da, downwind, anvil_th, anvil_top) = if has_anvil { self.anvil_distance(ut, v, y) } else { (1.0e6, 0.0, 0.0, top) };
        let mut env = if has_anvil { smin(dt, da, self.anvil_join.max(1.0)) } else { dt };
        let mut dov = 1.0e6;
        if self.over[1] > 1.0 {
            let oy = (y - top) * self.over[0] / self.over[1];
            dov = (ut * ut + v * v + oy * oy).sqrt() - self.over[0];
            env = smin(env, dov, OVERSHOOT_JOIN_M);
        }
        // The rain under the base, slanting downwind as it falls, its radius ragged up to a quarter more.
        let ur = u - self.rain[3] - self.rain[2] * (base - y).max(0.0);
        let rr = (ur * ur + v * v).sqrt();
        let skin_t = TOWER_SKIN_M.max(1.5 * footprint_m);
        let mut near = env - (self.reach() + 0.25 * skin_t + 120.0);
        if self.rain[1] > 0.0 {
            near = near.min((rr - 1.25 * self.rain[0]).max(y - base - BASE_RAG_M * (1.0 + BASE_RAG2)));
        }
        if near > 0.0 {
            return Read { density: 0.0, h01, free_m: near, safe: true, full: 0.0, rain: 0.0 };
        }
        let wa = ((dt - da) / 600.0 + 0.5).clamp(0.0, 1.0);
        // The anvil is the tower's outflow near it (dense, crisp and lumpy as the tower) and ice downwind.
        let ice = wa * smoothstep(ICE_FROM, ICE_BY, downwind);
        let mut surf = env;
        // The cauliflower, in the storm's own frame (it rides with it), on every face, scaled to its turret (the
        // updraft's tile on its mass, half on the flanking line), and a sixteenth of it where a texel resolves it; outside
        // the surface it fades to nothing at twice its height. The anvil's ice keeps its own envelope.
        // where the overshoot is the nearest body it is a cauliflower head
        let head = smoothstep(-200.0, 200.0, dt.min(da) - dov);
        if lod >= Lod::Near && ice < 0.999 {
            let flank = ((dm - df) / 400.0 + 0.5).clamp(0.0, 1.0);
            let c1 = self.lump_tile * (1.0 - (1.0 - HEAD_TILE) * head);
            let mut push = self.turret_lumps(tiles, u, y, v, footprint_m, c1, flank).0;
            if lod == Lod::View && footprint_m < 0.06 * LUMP_FINE * c1 {
                push += self.turret_lumps(tiles, u, y, v, footprint_m, LUMP_FINE * c1, flank).0;
            }
            let low_domes = smoothstep(base - BASE_RAG_M, base + BASE_DOMES_M, y);
            // a top with no anvil is bald (calvus)
            let bald = 1.0 - SMOOTH_TOP * self.smooth_top * smoothstep(SMOOTH_FROM, 0.9, h01);
            surf -= push * (1.0 - surf / (2.0 * self.lump_most)).clamp(0.0, 1.0) * (1.0 - ice) * (1.0 - PLATE_DOMES_OFF * wa * (1.0 - head)) * low_domes * bald;
        }
        // its faint upright striations
        if self.smooth_top > 0.0 && h01 > 0.8 * SMOOTH_FROM {
            let ps = self.place[0];
            let n = tiles.noise.shape.sample(u / STRIATION_TILE_M[0] + ps.z, y / STRIATION_TILE_M[1] + ps.x, v / STRIATION_TILE_M[0] + ps.y);
            surf -= STRIATION_M * (2.0 * n - 1.0) * self.smooth_top * smoothstep(SMOOTH_FROM, 0.9, h01);
        }
        // The anvil's fibres along its wind, moving its surface more toward its frayed far end (read only where the ice
        // has a share).
        let fray = ((downwind - FRAY_FROM) / (1.0 - FRAY_FROM)).clamp(0.0, 1.0);
        let mut fibre = 0.5;
        if ice > 0.001 {
            let pf = self.place[3];
            // across in shares of the plate's half width there: the fibres fan out with it from the tower
            let vn = v * self.anvil_near / (self.anvil_near + (self.anvil[1] - self.anvil_near) * downwind).max(1.0);
            let f1 = tiles.noise.shape.sample(ut / FIBRE_TILE_M[0] + pf.x, y / FIBRE_TILE_M[1] + pf.y, vn / FIBRE_TILE_M[2] + pf.z);
            let f2 = tiles.noise.shape.sample(ut / FIBRE_TILE2_M[0] + pf.y, y / FIBRE_TILE2_M[1] + pf.z, vn / FIBRE_TILE2_M[2] + pf.x);
            fibre = f1 + (f2 - f1) * FIBRE_FINE;
            surf -= FIBRE_M * (1.0 + FIBRE_FRAY * fray) * (2.0 * fibre - 1.0) * ice;
        }
        // The anvil's underside sags in slow lobes from the tower on (read only under the plate's middle).
        let low = wa * smoothstep(anvil_top - 0.35 * anvil_th, anvil_top - 0.75 * anvil_th, y);
        if low > 0.001 {
            let pu = self.place[0];
            let vu = v * self.anvil_near / (self.anvil_near + (self.anvil[1] - self.anvil_near) * downwind).max(1.0);
            let sag = tiles.noise.shape.sample(ut / UNDER_TILE_M[0] + pu.x, 0.29 + pu.y, vu / UNDER_TILE_M[1] + pu.z);
            surf -= UNDER_SAG * anvil_th * (2.0 * sag - 1.0).max(0.0) * low;
        }
        // Its top undulates from the tower on (read only near the plate's top).
        let high = wa * smoothstep(anvil_top - 0.4 * anvil_th, anvil_top - 0.1 * anvil_th, y);
        if high > 0.001 {
            let pu = self.place[0];
            let vt = v * self.anvil_near / (self.anvil_near + (self.anvil[1] - self.anvil_near) * downwind).max(1.0);
            let wave = tiles.noise.shape.sample(ut / ANVIL_TOP_TILE_M[0] + pu.y, 0.71 + pu.z, vt / ANVIL_TOP_TILE_M[1] + pu.x);
            surf -= ANVIL_TOP_WAVE * anvil_th * (2.0 * wave - 1.0) * high;
        }
        let base_y = self.base_at(tiles, u, y, v);
        let surface = surf.max((base_y - y) * BASE_SOFT);
        // The tower as cumulus cells over the domed surface (the GPU's den_t), a soft skin on the anvil. The light
        // marches' far reads see the profile squared alone, smooth across the band (one read a long step of the cells'
        // own field fell in a cell or a gap by the texel: salt and pepper on the shaded and backlit sides, the VDB study
        // 2026-11-11).
        let pc = self.place[1];
        let cs = CELL_SOFT_TILE * self.lump_tile;
        let prof = smoothstep(CELL_OUT_M, -CELL_IN_M, surf);
        let tower = if lod == Lod::Soft {
            prof * prof * smoothstep(0.0, CELL_BASE_M, y - base_y)
        } else {
            let wb = tiles.billow.sample(u / self.lump_tile + pc.z, y / self.lump_tile + pc.x, v / self.lump_tile + pc.y);
            let ws = tiles.noise.shape.sample(u / cs + pc.y + 0.31, y / cs + pc.z + 0.17, v / cs + pc.x + 0.53);
            let field = (wb + (ws - wb) * CELL_MIX) - CELL_FALL * (1.0 - prof);
            let mut cells = ((field - CELL_CUT) / CELL_RAMP).clamp(0.0, 1.0);
            if lod >= Lod::Near && cells > 0.0 && prof < 0.97 {
                let ce = FRAY_TILE * cs;
                let we = tiles.noise.shape.sample(u / ce + pc.x + 0.71, y / ce + pc.y + 0.29, v / ce + pc.z + 0.47);
                let ce2 = 0.33 * ce;
                let we2 = tiles.noise.shape.sample(u / ce2 + pc.z + 0.13, y / ce2 + pc.x + 0.83, v / ce2 + pc.y + 0.61);
                let wm = 0.6 * we + 0.4 * (we2 + (we - we2) * smoothstep(0.05 * ce2, 0.2 * ce2, footprint_m));
                let ek = FRAY * (1.0 - prof * prof) * (1.0 - wm) * (1.0 - smoothstep(0.05 * ce, 0.2 * ce, footprint_m));
                cells = ((cells - ek) / (1.0 - ek).max(0.05)).clamp(0.0, 1.0);
            }
            cells *= (CELL_THIN + (1.0 - CELL_THIN) * prof * prof) * smoothstep(CELL_OUT_M, 0.3 * CELL_OUT_M, surf);
            // (deeper than the cells the core is solid: their gaps showed a grey haze inside the tower)
            let core = smoothstep(-CELL_IN_M, -2.0 * CELL_IN_M, surf);
            cells.max(core) * smoothstep(0.0, CELL_BASE_M, y - base_y)
        };
        let anvil = (0.25 - surface / ANVIL_SKIN_M.max(1.5 * footprint_m)).clamp(0.0, 1.0);
        let mut density = tower + (anvil - tower) * ice;
        // The anvil's far end frays into the fibres' strands; its ice thins downwind and at its rims.
        let mut strands = 1.0;
        if fray > 0.001 {
            let pg = self.place[4];
            let vg = v * self.anvil_near / (self.anvil_near + (self.anvil[1] - self.anvil_near) * downwind).max(1.0);
            let gap = tiles.noise.shape.sample(ut / GAP_TILE_M[0] + pg.z, y / 600.0 + pg.x, vg / GAP_TILE_M[1] + pg.y);
            let patches = smoothstep(0.35, 0.6, gap) * (0.55 + 0.45 * smoothstep(0.3, 0.7, fibre));
            strands = 1.0 + (patches - 1.0) * fray * GAPS;
        }
        let body = ANVIL_DENSITY * (-ANVIL_DECAY * (downwind - ANVIL_DENSE).max(0.0)).exp() * (ANVIL_THIN + (1.0 - ANVIL_THIN) * smoothstep(0.0, 0.6 * self.anvil[2], -da));
        let mix = 1.0 + (body * strands - 1.0) * ice;
        density *= mix;
        let mut full = if y < base_y { 0.0 } else { mix };
        // The rain: soft streaks under the base, its sides ragged, down to the ground.
        let mut rain_drawn = 0.0;
        if self.rain[1] > 0.0 && y < base + 200.0 {
            let (rain, drawn) = self.rain_at(tiles, ur, rr, y, v, base_y);
            if y < base_y {
                full = rain;
                if drawn >= density {
                    rain_drawn = drawn;
                }
            }
            density = density.max(drawn);
        }
        if density > 0.0 {
            return Read { density, h01, free_m: 0.0, safe: false, full: full.max(density), rain: rain_drawn };
        }
        let mut free_m = 0.5 * (surf - 0.25 * skin_t).max(base_y - y).max(0.0);
        if self.rain[1] > 0.0 && rr < 1.25 * self.rain[0] && y < base + BASE_RAG_M * (1.0 + BASE_RAG2) {
            free_m = free_m.min(RAIN_STEP_M);
        }
        Read { density, h01, free_m, safe: false, full: 0.0, rain: 0.0 }
    }

    /// The ragged base's height over (u, v), in the storm's frame, for a point `y` high: flat as a whole and ragged at its
    /// edge, the noise never pushing cloud under it (read only near the base: higher, the base's term is under any
    /// cloud's and the density is full there; elsewhere its lowest point).
    #[inline]
    fn base_at(&self, tiles: &Tiles, u: f32, y: f32, v: f32) -> f32 {
        if y < self.base + 4.0 * BASE_RAG_M {
            let pb = self.place[2];
            let rag = tiles.noise.shape.sample(u / BASE_RAG_TILE_M + pb.x, pb.y, v / BASE_RAG_TILE_M + pb.z);
            let torn = tiles.noise.shape.sample(u / BASE_RAG_TILE2_M + pb.z, y / BASE_RAG_TILE2_M + pb.x, v / BASE_RAG_TILE2_M + pb.y);
            return self.base + BASE_RAG_M * ((2.0 * rag - 1.0) + BASE_RAG2 * (2.0 * torn - 1.0));
        }
        self.base - BASE_RAG_M * (1.0 + BASE_RAG2)
    }

    /// The rain at (`ur` along its slanted axis, `y` up, `v` across), `rr` from its axis, under the ragged base `base_y`:
    /// its density deep in it (the march's step rule) and as drawn through its curtains. Soft streaks under the base,
    /// its sides ragged, down to the ground.
    #[inline]
    fn rain_at(&self, tiles: &Tiles, ur: f32, rr: f32, y: f32, v: f32, base_y: f32) -> (f32, f32) {
        let o = self.place[4];
        let cores = tiles.noise.detail.sample(ur / RAIN_TILE_M[0] + o.x, y / RAIN_TILE_M[1] + o.y, v / RAIN_TILE_M[2] + o.z);
        let streaks = tiles.noise.detail.sample(RAIN_STREAK * ur / RAIN_TILE_M[0] + o.y, y / RAIN_TILE_M[1] + o.z, RAIN_STREAK * v / RAIN_TILE_M[2] + o.x);
        let curtain = 0.6 * cores + 0.4 * streaks;
        // separate shafts, some ending above the ground (the GPU's cb_an, cb_ao)
        let sh = tiles.noise.shape.sample(ur / RAIN_SHAFT_M + o.y + 0.53, y / (4.0 * RAIN_SHAFT_M) + o.x, v / RAIN_SHAFT_M + o.z + 0.29);
        let shafts = smoothstep(RAIN_SHAFT_CUT, RAIN_SHAFT_CUT + 0.2, sh);
        let ce = RAIN_END_TILE * RAIN_SHAFT_M;
        let en = tiles.noise.shape.sample(ur / ce + o.z + 0.83, 0.47, v / ce + o.y + 0.11);
        let end_y = RAIN_END * self.base * smoothstep(RAIN_END_FROM, RAIN_END_FROM + 0.15, en);
        // (faded in under its end, so a shaft that reaches the ground is full there: fading in over the lowest 400 m, every
        // shaft thinned to nothing at the ground and the horizon shone through under the curtain as a white band)
        let ends = smoothstep(end_y - RAIN_END_FADE_M, end_y, y);
        let across = 1.0 - smoothstep(0.1 * self.rain[0], self.rain[0], rr * (0.7 + 0.6 * cores));
        let fall = smoothstep(-150.0, 100.0, self.base - y) * (RAIN_GROUND + (1.0 - RAIN_GROUND) * smoothstep(0.0, self.base, y));
        let virga = 1.0 + VIRGA * (1.0 - smoothstep(0.0, VIRGA_M, base_y - y));
        let rain = RAIN_THIN * RAIN_SHARE * self.rain[1] * across * fall * virga * shafts * ends;
        (rain, rain * smoothstep(0.25, 0.75, curtain))
    }

    /// `read`'s density and rain at `p` (a texel `footprint_m` wide) where no cloud can be: under the ragged base's
    /// lowest point by more than the skins (its distance's base term then holds the tower's and the anvil's density at 0,
    /// and the rain is the body), the rain alone, without the tower's, the anvil's and the base's reads; deeper than
    /// VIRGA_M under that point, without the base's either (the virga is gone there whatever its height). None
    /// elsewhere: read it there.
    fn rain_only(&self, tiles: &Tiles, p: Vec3f, footprint_m: f32) -> Option<f32> {
        let (u, y, v) = self.local(p);
        let lowest = self.base - BASE_RAG_M * (1.0 + BASE_RAG2);
        if !(y < lowest - TOWER_SKIN_M.max(ANVIL_SKIN_M).max(1.5 * footprint_m)) {
            return None;
        }
        let ur = u - self.rain[3] - self.rain[2] * (self.base - y).max(0.0);
        let rr = (ur * ur + v * v).sqrt();
        // (past 1.25 of its radius even its ragged side holds none)
        if !(self.rain[1] > 0.0) || !(rr < 1.25 * self.rain[0]) {
            return Some(0.0);
        }
        let base_y = if y < lowest - VIRGA_M { lowest } else { self.base_at(tiles, u, y, v) };
        Some(self.rain_at(tiles, ur, rr, y, v, base_y).1)
    }

    /// The tower's distance at `p` without the noise (the rain's shade reads it where its sun ray meets the base).
    fn envelope_distance(&self, p: Vec3f) -> f32 {
        let (u, y, v) = self.local(p);
        let ul = u - self.lean * (y - self.base).clamp(0.0, self.top - self.base);
        let (dm, df) = self.tower(ul, v, y);
        smin(dm, df, LINE_JOIN * self.radius)
    }

    /// Where the unit direction `d` (d.y > 0) from the eye is inside the bounds: (near, far) metres, None where it
    /// misses them.
    fn span(&self, d: Vec3f) -> Option<(f32, f32)> {
        let (tower, anvil, anvil_floor) = self.bounds();
        let ceiling = self.ceiling();
        match (upright_span(d, tower, 0.0, ceiling), upright_span(d, anvil, anvil_floor, ceiling)) {
            (Some(a), Some(b)) => Some((a.0.min(b.0), a.1.max(b.1))),
            (a, b) => a.or(b),
        }
    }

    /// The upload's uniforms (the Cumulonimbus export shader's section reads them by these names).
    /// `drops`: its rain takes the sun's light by the drops' phase, beside the march (cb_ai.w).
    fn uniforms(&self, out: &mut crate::cloud_volume_gpu::UploadBuilder, drops: bool) {
        let (tower, anvil, anvil_floor) = self.bounds();
        out.uniform("cb_a", [self.centre.x, self.centre.y, self.base, self.top]);
        out.uniform("cb_b", [self.wind.x, self.wind.y, self.radius, self.lean]);
        out.uniform("cb_c", self.anvil);
        out.uniform("cb_d", [self.anvil_near, self.over[0], self.over[1], self.anvil_join]);
        out.uniform("cb_e", self.rain);
        out.uniform("cb_f", [self.sigma, 0.0, self.lump_tile, self.lump_most]);
        let p = |k: usize| [self.place[k].x, self.place[k].y, self.place[k].z, 0.0];
        out.uniform("cb_g", p(0));
        out.uniform("cb_h", p(1));
        out.uniform("cb_i", p(2));
        out.uniform("cb_j", p(3));
        out.uniform("cb_m", p(4));
        out.uniform("cb_o", [CB_POWDER, RAIN_SHADE_M, RAIN_STEP_M, CB_SUN_SHARE]);
        out.uniform("cb_p", [TAPER, FRAY_FROM, CURL_SHARE, THIN_END]);
        out.uniform("cb_q", [PUFF_JOIN, BASE_RAG_M, BASE_RAG_TILE_M, RAIN_SHADE_EDGE_M]);
        out.uniform("cb_r", [0.0, FIBRE_FRAY, ANVIL_DENSITY, ANVIL_THIN]);
        out.uniform("cb_ac", [ANVIL_DENSE, STORM_DIFFUSION, RIM, self.dark]);
        out.uniform("cb_ai", [RIM_SHADOW, RIM_FROM, RIM_TO, if drops { 1.0 } else { 0.0 }]);
        out.uniform("cb_ad", [ANVIL_DECAY, ANVIL_DROOP, ANVIL_TOP_WAVE, ANVIL_FALL]);
        out.uniform("cb_af", [BASE_RAG2, BASE_RAG_TILE2_M, VIRGA, VIRGA_M]);
        out.uniform("cb_ag", [STRIATION_M, STRIATION_TILE_M[0], STRIATION_TILE_M[1], self.smooth_top]);
        out.uniform("cb_ah", [UNDER_TILE_M[0], UNDER_TILE_M[1], ANVIL_TOP_TILE_M[0], ANVIL_TOP_TILE_M[1]]);
        out.uniform("cb_y", [CAULI_CAP1, CAULI_CAP2, CAULI_RIDE, self.dome_height]);
        out.uniform("cb_z", [CAULI_HEIGHT2, CREVICE_SHADE, RAIN_OPEN, FIBRE_FINE]);
        out.uniform("cb_s", [CAULI_SOFT, TOWER_SKIN_M, UNDER_SAG, PLATE_DOMES_OFF]);
        out.uniform("cb_ae", [CELL_SOFT_TILE, CELL_IN_M, CELL_OUT_M, SUN_DOME_STEPS]);
        out.uniform("cb_al", [CELL_MIX, CELL_CUT, CELL_RAMP, CELL_BASE_M]);
        out.uniform("cb_am", [CELL_FALL, CELL_THIN, FRAY, FRAY_TILE]);
        out.uniform("cb_an", [RAIN_SHAFT_M, RAIN_SHAFT_CUT, RAIN_END, RAIN_THIN]);
        out.uniform("cb_ao", [RAIN_END_TILE, RAIN_END_FADE_M, RAIN_END_FROM, 0.0]);
        out.uniform("cb_aj", [HEAD_TILE, 0.0, GAP_TILE_M[0], GAP_TILE_M[1]]);
        out.uniform("cb_ak", [GAPS, UP_AGAIN_M, PAIRS_TO, 0.0]);
        out.uniform("cb_aa", [UP_SOFT_M, TWO_MARCHES, UP_DEPTH, UP_TILT]);
        out.uniform("cb_ab", [BASE_DOMES_M, DARK_TO, SMOOTH_FROM, SMOOTH_TOP]);
        out.uniform("cb_v", [RAIN_SHARE, RAIN_GROUND, FIBRE_M, ANVIL_SKIN_M]);
        out.uniform("cb_w", [UPDRAFT_TAPER, ICE_FROM, ICE_BY, ANVIL_JOIN]);
        out.uniform("cb_x", [BASE_SOFT, COLUMN_SHARE, UPDRAFT_COLUMN, LINE_JOIN]);
        out.uniform("cb_k", [tower[0], tower[1], tower[2], self.ceiling()]);
        out.uniform("cb_l", [anvil[0], anvil[1], anvil[2], anvil_floor]);
        let names = ["cb_t0", "cb_t1", "cb_t2", "cb_t3", "cb_t4", "cb_t5", "cb_t6"];
        for (name, t) in names.into_iter().zip(&self.turrets) {
            out.uniform(name, *t);
        }
        let names = [
            "cb_u0", "cb_u1", "cb_u2", "cb_u3", "cb_u4", "cb_u5", "cb_u6", "cb_u7", "cb_u8", "cb_u9", "cb_u10", "cb_u11", "cb_u12", "cb_u13", "cb_u14", "cb_u15", "cb_u16", "cb_u17",
        ];
        for (name, s) in names.into_iter().zip(&self.puffs) {
            out.uniform(name, *s);
        }
    }

    /// Every number it draws from, for keys.
    fn fields(&self) -> Vec<f32> {
        let mut f = vec![self.centre.x, self.centre.y, self.wind.x, self.wind.y, self.base, self.top, self.radius, self.lean, self.anvil_near, self.over[0], self.over[1], self.anvil_join, self.smooth_top, self.dark, self.sigma, self.lump_tile, self.dome_height, self.lump_most];
        f.extend_from_slice(&self.anvil);
        f.extend_from_slice(&self.rain);
        for t in &self.turrets {
            f.extend_from_slice(t);
        }
        for s in &self.puffs {
            f.extend_from_slice(s);
        }
        for p in &self.place {
            f.extend_from_slice(&[p.x, p.y, p.z]);
        }
        f
    }
}

/// Where the unit direction `d` (d.y > 0) from the eye is inside the upright cylinder `c` (x, z, radius) between the
/// heights `floor` and `ceiling`: (near, far) metres, None where it misses it.
fn upright_span(d: Vec3f, c: [f32; 3], floor: f32, ceiling: f32) -> Option<(f32, f32)> {
    let a = d.x * d.x + d.z * d.z;
    let b = d.x * c[0] + d.z * c[1];
    let cc = c[0] * c[0] + c[1] * c[1] - c[2] * c[2];
    let (t0, t1) = if a > 1.0e-9 {
        let disc = b * b - a * cc;
        if disc < 0.0 {
            return None;
        }
        let s = disc.sqrt();
        ((b - s) / a, (b + s) / a)
    } else if cc > 0.0 {
        return None;
    } else {
        (0.0, f32::INFINITY)
    };
    let lo = if floor > 2.5 { shell_distance(d.y, floor) } else { 0.0 };
    let hi = shell_distance(d.y, ceiling);
    let (near, far) = (t0.max(lo).max(0.0), t1.min(hi));
    (far > near).then_some((near, far))
}

/// A placed volume's shape: a storm's envelope.
enum Shape {
    Storm(Envelope),
}

impl Shape {
    fn read(&self, tiles: &Tiles, p: Vec3f, footprint_m: f32, lod: Lod) -> Read {
        match self {
            Shape::Storm(e) => e.read(tiles, p, footprint_m, lod),
        }
    }

    fn span(&self, d: Vec3f) -> Option<(f32, f32)> {
        match self {
            Shape::Storm(e) => e.span(d),
        }
    }

    /// Extinction per metre at density 1.
    fn sigma(&self) -> f32 {
        match self {
            Shape::Storm(e) => e.sigma,
        }
    }

    /// Its base and its top (the light pass's two sun colours).
    fn base_top(&self) -> (f32, f32) {
        match self {
            Shape::Storm(e) => (e.base, e.top),
        }
    }

    /// The heights everything it draws stands between: a storm's rain reaches the ground.
    fn floor_ceiling(&self) -> (f32, f32) {
        match self {
            Shape::Storm(e) => (0.0, e.ceiling()),
        }
    }

    /// The upright cylinders that hold it, each (x, z, radius) with its floor: a storm's tower (from the ground) and
    /// anvil.
    fn cylinders(&self) -> Vec<([f32; 3], f32)> {
        match self {
            Shape::Storm(e) => {
                let (tower, anvil, anvil_floor) = e.bounds();
                vec![(tower, 0.0), (anvil, anvil_floor)]
            }
        }
    }

    fn fields(&self) -> Vec<f32> {
        match self {
            Shape::Storm(e) => e.fields(),
        }
    }

    fn uniforms(&self, out: &mut crate::cloud_volume_gpu::UploadBuilder, drops: bool) {
        match self {
            Shape::Storm(e) => e.uniforms(out, drops),
        }
    }

    /// Its cloud type: the export shader its GPU march draws with (design §12.13).
    fn genus(&self) -> CloudGenus {
        match self {
            Shape::Storm(_) => CloudGenus::Cumulonimbus,
        }
    }
}

/// A placed cloud volume, ready to march (design §2.1's placed class): a Cumulonimbus.
pub struct PlacedVolume {
    shape: Shape,
    tiles: Tiles,
    sun: Vec3f,
    lining: f32,
    stream: u32,
    key: u64,
    /// Its rain's phase under the map's sun (`add_bow`): None sends no sunlight from the rain.
    bow: Option<Box<Bow>>,
    /// The storm's shadow on its rain over the base plane, for the rain's sunlight (`add_bow`).
    shade: Option<ShadeGrid>,
    /// The map's Rotation (its rotation_deg, `add_bow`): a bake's look along its texel centre reaches the layers turned
    /// back by it (lib.rs `radiance_with`), so the rain's tables are laid out at the texel centres turned so.
    turn_deg: f32,
    /// The rain's sunlight marched at the texels of the bake widths and segments looked up so far (`rain_seen`).
    rain: [OnceLock<RainTable>; RAIN_TABLES],
    /// Whether the tables ran out (logged once).
    rain_full: AtomicBool,
}

/// The sunlit rain (`PlacedVolume::rain_column`) of march segment `k` at the texel centres of a map `width` texels wide
/// (turned back by the map's Rotation as its looks are, `turn_deg`), each cell marched the first time a look reads it
/// and kept (its f32 bits; RAIN_UNMARCHED until then): a bake's looks,
/// its antialiasing passes' jittered looks and the puddles' mirrored looks read the same marches instead of marching
/// the shaft each. A row is made when a look first reaches it (the rows the storm stands in). A cell is its texel's
/// march whichever thread makes it, so the maps stay byte-repeatable.
struct RainTable {
    width: usize,
    height: usize,
    k: usize,
    cuts: Vec<f32>,
    rows: Box<[OnceLock<Box<[AtomicU32]>>]>,
}

impl RainTable {
    fn new(width: usize, k: usize, cuts: &[f32]) -> RainTable {
        let height = (width / 2).max(1);
        RainTable { width, height, k, cuts: cuts.to_vec(), rows: (0..height).map(|_| OnceLock::new()).collect() }
    }

    fn holds(&self, width: usize, k: usize, cuts: &[f32]) -> bool {
        self.width == width && self.k == k && self.cuts[..] == *cuts
    }
}

/// A table coordinate (cells, a cell's centre at a whole number) as its cell and the share of the next one: within
/// RAIN_SNAP of a centre that cell alone.
fn rain_texel(f: f32) -> (i64, f32) {
    let i = f.floor();
    match f - i {
        t if t < RAIN_SNAP => (i as i64, 0.0),
        t if t > 1.0 - RAIN_SNAP => (i as i64 + 1, 0.0),
        t => (i as i64, t),
    }
}

/// The storm's shadow on its rain as `sun_depth` reads it under the lowest base (the envelope's distance where the sun's
/// ray meets the base, lit across RAIN_SHADE_EDGE_M), tabled over the base plane in SHADE_CELL_M cells across the
/// tower's bounds and read bilinearly: the rain's sunlight reads it at every step of every look into the rain
/// (`rain_column`), where reading the envelope (its turrets and their puffs) each time doubled a map's bake. Lit outside
/// the bounds, where the distance is past the edge.
struct ShadeGrid {
    x0: f32,
    z0: f32,
    n: usize,
    lit: Vec<f32>,
}

impl ShadeGrid {
    fn of(e: &Envelope) -> ShadeGrid {
        let (tower, _, _) = e.bounds();
        let n = (2.0 * tower[2] / SHADE_CELL_M).ceil() as usize + 2;
        let (x0, z0) = (tower[0] - tower[2], tower[1] - tower[2]);
        let y = e.base + RAIN_SHADE_M;
        let lit = (0..n * n)
            .map(|k| {
                let at = vec3f(x0 + (k % n) as f32 * SHADE_CELL_M, y, z0 + (k / n) as f32 * SHADE_CELL_M);
                smoothstep(-RAIN_SHADE_EDGE_M, RAIN_SHADE_EDGE_M, e.envelope_distance(at))
            })
            .collect();
        ShadeGrid { x0, z0, n, lit }
    }

    /// How lit the base plane is at (x, z).
    fn lit_at(&self, x: f32, z: f32) -> f32 {
        let (fx, fz) = ((x - self.x0) / SHADE_CELL_M, (z - self.z0) / SHADE_CELL_M);
        let last = (self.n - 1) as f32;
        if !(fx >= 0.0 && fz >= 0.0 && fx < last && fz < last) {
            return 1.0;
        }
        let (i, j) = (fx as usize, fz as usize);
        let (tx, tz) = (fx - i as f32, fz - j as f32);
        let at = |i: usize, j: usize| self.lit[j * self.n + i];
        let a = at(i, j) + (at(i + 1, j) - at(i, j)) * tx;
        let b = at(i, j + 1) + (at(i + 1, j + 1) - at(i, j + 1)) * tx;
        a + (b - a) * tz
    }
}

/// What a placed volume is built from (the field volumes' `VolumeSpec`'s fields it reads).
#[derive(Clone, Copy)]
pub struct PlacedSpec<'a> {
    pub layer: &'a CloudLayerParams,
    pub genus: CloudGenus,
    pub cover: f32,
    pub map_seed: u32,
    pub downwind: Vec2f,
    pub sun_dir: Vec3f,
}

impl PlacedVolume {
    /// The volume of `spec`: None for a type this renderer does not place, no cover or no density. The seed's noise
    /// tiles are looked up or built (`cancelled` polled, rows spread by `run`).
    pub fn build(spec: &PlacedSpec, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<Option<PlacedVolume>, Cancelled> {
        let density = match spec.genus {
            CloudGenus::Cumulonimbus => spec.layer.cumulonimbus_params().density,
            _ => return Ok(None),
        };
        if !(spec.cover > 0.0) || !(density > 0.0) {
            return Ok(None);
        }
        let stream = cloud_layer_seed(spec.map_seed, spec.layer.seed, spec.genus);
        let wind = if spec.downwind.length() > 0.5 { spec.downwind.normalize() } else { vec2f(1.0, 0.0) };
        let shape = Shape::Storm(Envelope::of(spec.layer, spec.cover, stream, wind));
        let noise = noise_for(spec.map_seed, cancelled, run)?;
        let billow = noise.billow(cancelled, run)?;
        let sun = spec.sun_dir.normalize();
        let sun = if sun.is_finite() && sun.length() > 0.5 { sun } else { vec3f(0.0, 1.0, 0.0) };
        let lining = spec.layer.lining.unwrap_or(spec.genus.default_lining());
        let bits = f32::from_bits;
        let mut fields = shape.fields();
        fields.extend_from_slice(&[sun.x, sun.y, sun.z, bits(spec.map_seed), spec.genus as u32 as f32]);
        let key = footprint_grid_key(&fields, stream, TURRETS, GPU_KEY_PART);
        Ok(Some(PlacedVolume {
            shape,
            tiles: Tiles { noise, billow },
            sun,
            lining,
            stream,
            key,
            bow: None,
            shade: None,
            turn_deg: 0.0,
            rain: std::array::from_fn(|_| OnceLock::new()),
            rain_full: AtomicBool::new(false),
        }))
    }

    /// The heights a march of it spans, metres: a storm's from the ground (its rain) to its ceiling. The walk places it by these and cuts it at the flat entries between.
    pub fn shell_m(&self) -> (f32, f32) {
        self.shape.floor_ceiling()
    }

    /// Its base and its top: the light pass lights it at these (design §2.3's two sun colours).
    pub fn light_shell_m(&self) -> (f32, f32) {
        self.shape.base_top()
    }

    /// Its Silver lining.
    pub fn lining(&self) -> f32 {
        self.lining
    }

    /// The box of sky directions its bounds hold, degrees: (azimuth from, azimuth to, elevation from, elevation to), the
    /// azimuth unwrapped round its first cylinder's (it may run under 0 or over 360), half a degree of margin round it
    /// (the Earth's curve lowers a far point a little). The whole sky's box for a viewer inside its bounds.
    pub fn sky_box(&self) -> [f32; 4] {
        let ceiling = self.shape.floor_ceiling().1;
        let mut out: Option<[f32; 4]> = None;
        for ([x, z, r], floor) in self.shape.cylinders() {
            let d = (x * x + z * z).sqrt();
            if !(d > r) {
                return [0.0, 360.0, 0.0, 90.0];
            }
            let mut az = x.atan2(-z).to_degrees();
            let half = (r / d).asin().to_degrees();
            if let Some(b) = out {
                let mid = 0.5 * (b[0] + b[1]);
                az = mid + (az - mid + 540.0).rem_euclid(360.0) - 180.0;
            }
            let (lo, hi) = (floor.atan2(d + r).to_degrees(), ceiling.atan2(d - r).to_degrees());
            let b = out.unwrap_or([az - half, az + half, lo, hi]);
            out = Some([b[0].min(az - half), b[1].max(az + half), b[2].min(lo), b[3].max(hi)]);
        }
        let b = out.unwrap_or([0.0, 360.0, 0.0, 90.0]);
        [b[0] - 0.5, b[1] + 0.5, (b[2] - 0.5).max(0.01), (b[3] + 0.5).min(90.0)]
    }

    /// The lit march along `d` at a bake `width` wide, cut at the heights `cuts` into `out`'s records (as
    /// `CloudVolume::march`): rough, the fallback's and the preview's. In air it moves by the envelope's free distance;
    /// in cloud about CPU_STEP_PATHS of a mean free path of the body it is in a step; each lit step takes a sun march of
    /// CPU_SUN_STEPS and an up march of CPU_UP_STEPS through the light density.
    pub fn march(&self, d: Vec3f, width: usize, cuts: &[f32], out: &mut [VolumeMarch]) {
        for m in out.iter_mut() {
            *m = VolumeMarch::CLEAR;
        }
        let length = d.length();
        if out.is_empty() || !(length > 0.0) {
            return;
        }
        let d = d / length;
        if !(d.y > 0.0) {
            return;
        }
        let Some((near, far)) = self.shape.span(d) else {
            return;
        };
        let cuts = &cuts[..cuts.len().min(out.len() - 1)];
        let angle = std::f32::consts::TAU / width.max(LAW_WIDTH_LEAST) as f32;
        let light = RayLight::new(d.dot(self.sun), self.sun.y, self.lining, LightFamily::Heap).seen_from(d.y);
        // The rain under the base takes the sun's light by the drops' phase (`rain_light`, beside the march) once it has
        // them (`add_bow`): here only the dome's, the open side's and the ground's.
        let drops = self.bow.is_some();
        let mut rain_lit = light;
        (rain_lit.forward, rain_lit.back, rain_lit.diffusion, rain_lit.diffuse) = (Default::default(), Default::default(), Default::default(), 0.0);
        let width = width.max(2);
        let uv = ibl::dir_to_equirect_uv(arr(d));
        let jitter = blue_jitter((uv[0] * width as f32) as u32, (uv[1] * (width / 2) as f32) as u32);
        let (mut seg, mut front) = (0usize, 1.0f32);
        let mut next_cut = cuts.first().map_or(f32::INFINITY, |&h| shell_distance(d.y, h));
        let mut t = near + jitter * (near * angle).clamp(10.0, 120.0);
        let sigma = self.shape.sigma();
        let rain_under = match &self.shape {
            Shape::Storm(e) => e.base,
        };
        while t < far {
            let p = vec3f(d.x * t, height_at(t, d.y), d.z * t);
            let footprint = t * angle;
            let r = self.shape.read(&self.tiles, p, footprint, Lod::View);
            if !(r.density > 0.0) {
                let fine = (0.5 * footprint).max(4.0);
                t += if r.safe { r.free_m.max(footprint.clamp(4.0, 60.0)) } else { r.free_m.clamp(fine, 6.0 * fine) };
                continue;
            }
            while t >= next_cut {
                front *= out[seg].transmittance;
                seg += 1;
                next_cut = cuts.get(seg).map_or(f32::INFINITY, |&h| shell_distance(d.y, h));
            }
            let sig = r.density * sigma;
            // a share of a mean free path of the body the sample is in, never by its own density (a first sample in a
            // skin stepped deep into the dark core), at most RAIN_STEP_M in the rain
            let most = if p.y < rain_under { RAIN_STEP_M } else { CPU_STEP_MOST_M };
            let ds = (CPU_STEP_PATHS / (r.full * sigma)).clamp((0.5 * footprint).max(4.0), most.max((0.5 * footprint).max(4.0))).min(far - t).max(0.5);
            let rain = drops && r.rain > 0.0;
            let (tau_sun, local) = if rain { (0.0, 0.0) } else { self.sun_depth(p, footprint) };
            let tau_up = self.up_depth(p, footprint);
            step(&mut out[seg], if rain { &rain_lit } else { &light }, sig, ds, tau_sun, 0.0, tau_up, local, r.h01, t, front);
            if out[seg].stops(front) {
                out[seg].close();
                return;
            }
            t += ds;
        }
    }

    /// Gives its rain the drops' phase (rainbow.rs) under the sun whose light at the ground is `sun`: the drops by
    /// Marshall and Palmer at the shaft's rate (its Rain shaft as the weather's Rain, rain_water.rs), none without a
    /// shaft or with the sun down (the rain then has no sunlight, as under a storm's shadow). The sunlight its rain sends
    /// is `rain_light`, beside the march and its GPU record, read from tables laid out at the texel centres of a map
    /// turned by `turn_deg`, the map's Rotation.
    pub(crate) fn add_bow(&mut self, sun: &SunBands, turn_deg: f32) {
        self.turn_deg = turn_deg;
        let Shape::Storm(e) = &self.shape;
        if e.rain[1] > 0.0 && self.sun.y > 0.0 {
            self.bow = Some(Box::new(Bow::new(Drops::rain(crate::rain_water::rate_mm_h(e.rain[1])), sun)));
            self.shade = Some(ShadeGrid::of(e));
        }
    }

    /// The sunlight its rain shaft sends along the unit `d` in segment `k` of a march cut at the heights `cuts`, for a
    /// bake `width` wide whose texels are `texel` radians across, as the sun's light at the ground shows it, before what
    /// stands over the storm and the haze in front (the stack applies those as to the storm's own light): the drops'
    /// single scattering by their own phase (`Bow::scattered`: the rainbow and its secondary, averaged over the texel,
    /// and the light reflected off the drops and refracted through them), not the light law's cloud lobes, which sent
    /// the rain 0.08-0.1 a steradian backward where drops send 0.0008 outside their bows (a pale sunlit curtain
    /// opposite the sun, the bow a +10 % ripple on it). Rain a few optical depths deep turns little of its light round:
    /// its drops send 44 % of what they take forward within 83 degrees and half stays in the beam. The phase at the
    /// look's angle from the sun holds all along the look, so this is that phase times the sunlit rain the look sees
    /// (`rain_column`, read from its table: `rain_seen`): the bow brightest where the look runs long through sunlit rain
    /// (low in the shaft, the storm's shadow on its upper part), fading where the shaft thins or ends, and nothing under
    /// the horizon, where the shaft stands on the ground at its distance.
    pub(crate) fn rain_light(&self, d: Vec3f, width: usize, cuts: &[f32], k: usize, texel: f32) -> Vec3f {
        let Some(bow) = self.bow.as_deref() else {
            return Vec3f::default();
        };
        let column = self.rain_seen(d, width, cuts, k);
        if !(column > 0.0) {
            return Vec3f::default();
        }
        bow.scattered(d.dot(self.sun), texel) * column
    }

    /// The sunlit rain along the unit `d` in segment `k` for a bake `width` wide (`rain_column`), read bilinearly from
    /// the table as wide (half as wide past RAIN_FULL_WIDTH) between the texel centres its cells were marched at, each
    /// from its own blue-noise offset: a look at a cell's centre reads exactly that march (an unjittered bake as wide as
    /// the table reads its texels' marches as before, at any Rotation: the look is turned back to the bake's texels),
    /// a jittered or mirrored look the four round it. A look just over the horizon reads the last row over it (the rows
    /// under it hold no rain). Where every table is taken, the look marches the cells it reads itself, unkept.
    fn rain_seen(&self, d: Vec3f, width: usize, cuts: &[f32], k: usize) -> f32 {
        if self.rain_span(d, cuts, k).is_none() {
            return 0.0;
        }
        let width = width.max(2);
        let w = if width <= RAIN_FULL_WIDTH { width } else { width / 2 };
        let table = self.rain_table(w, k, cuts);
        let h = (w / 2).max(1);
        let last = (h / 2).max(1) - 1;
        // where the look lies among the bake's texels: the map's Rotation turned back (the elevation stays)
        let at = if self.turn_deg == 0.0 { d } else { crate::rotate_y(d, self.turn_deg) };
        let uv = ibl::dir_to_equirect_uv(arr(at));
        let (x, tx) = rain_texel(uv[0] * w as f32 - 0.5);
        let (y, ty) = rain_texel((uv[1] * h as f32 - 0.5).clamp(0.0, last as f32));
        let mut column = 0.0f32;
        for (j, wy) in [(y, 1.0 - ty), (y + 1, ty)] {
            for (i, wx) in [(x, 1.0 - tx), (x + 1, tx)] {
                if wy * wx != 0.0 {
                    let j = (j.max(0) as usize).min(last);
                    let i = i.rem_euclid(w as i64) as usize;
                    column += wy * wx * match table {
                        Some(table) => self.rain_cell(table, i, j),
                        None => self.rain_march(w, h, cuts, k, i, j),
                    };
                }
            }
        }
        column
    }

    /// The table for bakes `width` wide in segment `k` of a march cut at `cuts`, made in a free slot the first time; None
    /// when all RAIN_TABLES hold others (logged once).
    fn rain_table(&self, width: usize, k: usize, cuts: &[f32]) -> Option<&RainTable> {
        for slot in &self.rain {
            let table = slot.get_or_init(|| RainTable::new(width, k, cuts));
            if table.holds(width, k, cuts) {
                return Some(table);
            }
        }
        if !self.rain_full.swap(true, Ordering::Relaxed) {
            log!("hdri clouds: a storm's rain keeps {RAIN_TABLES} tables of its sunlight; the looks past them march their cells unkept");
        }
        None
    }

    /// The table's cell (`i`, `j`): the march of texel (i, j) of a bake the table's width wide, along that bake's look at
    /// its texel centre (`bake_par_hot`'s arithmetic, turned by the map's Rotation as `radiance_with` turns it, so the
    /// same look bit for bit), marched the first time a look reads it.
    fn rain_cell(&self, table: &RainTable, i: usize, j: usize) -> f32 {
        let row = table.rows[j].get_or_init(|| (0..table.width).map(|_| AtomicU32::new(RAIN_UNMARCHED)).collect());
        let bits = row[i].load(Ordering::Relaxed);
        if bits != RAIN_UNMARCHED {
            return f32::from_bits(bits);
        }
        let column = self.rain_march(table.width, table.height, &table.cuts, table.k, i, j);
        row[i].store(column.to_bits(), Ordering::Relaxed);
        column
    }

    /// The march of texel (`i`, `j`) of a bake `width` x `height` (a table's cell, `rain_cell`, or one past the tables):
    /// along that bake's look at its texel centre, turned by the map's Rotation as `radiance_with` turns it.
    fn rain_march(&self, width: usize, height: usize, cuts: &[f32], k: usize, i: usize, j: usize) -> f32 {
        let uv = [(i as f32 + 0.5) / width as f32, (j as f32 + 0.5) / height as f32];
        let look = crate::rotate_y(crate::vec(ibl::equirect_uv_to_dir(uv)), -self.turn_deg);
        self.rain_column(look, width, cuts, k)
    }

    /// Where the unit `d` runs through the shaft's upright cylinder under its highest ragged base within segment `k` of a
    /// march cut at `cuts`: (near, far) metres; None without a shaft, at and under the horizon and where it misses.
    fn rain_span(&self, d: Vec3f, cuts: &[f32], k: usize) -> Option<(f32, f32)> {
        let Shape::Storm(e) = &self.shape;
        if !(e.rain[1] > 0.0) || !(d.y > 0.0) {
            return None;
        }
        // the shaft's axis slants downwind RAIN_SLANT a metre down from under its foot: the cylinder round its middle
        let top = e.base + BASE_RAG_M * (1.0 + BASE_RAG2);
        let mid = e.rain[3] + 0.5 * e.rain[2] * top;
        let c = [e.centre.x + e.wind.x * mid, e.centre.y + e.wind.y * mid, 1.25 * e.rain[0] + 0.5 * e.rain[2] * top];
        let (near, far) = upright_span(d, c, 0.0, top)?;
        let from = if k == 0 { 0.0 } else { cuts.get(k - 1).map_or(f32::INFINITY, |&h| shell_distance(d.y, h)) };
        let to = cuts.get(k).map_or(f32::INFINITY, |&h| shell_distance(d.y, h));
        let (near, far) = (near.max(from), far.min(to));
        (far > near).then_some((near, far))
    }

    /// The sunlit rain seen along the unit `d` in segment `k` (`rain_light`): the integral over the look of the rain's
    /// extinction (its density as the march reads it, `Read::rain`), times the sun that reaches it as the shaft's light
    /// takes it (`sun_depth`: the storm's shadow on the rain), seen through what of the segment lies in front of it, the
    /// rain at BOW_PASS (its diffraction stays in the bow's light) and cloud whole. Marched through the shaft's upright
    /// cylinder under its highest ragged base (`rain_span`) in RAIN_STEP_M steps from the texel's blue-noise offset (the
    /// march's jitter), stopping where the segment passes too little to see.
    fn rain_column(&self, d: Vec3f, width: usize, cuts: &[f32], k: usize) -> f32 {
        let Some((near, far)) = self.rain_span(d, cuts, k) else {
            return 0.0;
        };
        let Shape::Storm(e) = &self.shape;
        let angle = std::f32::consts::TAU / width.max(LAW_WIDTH_LEAST) as f32;
        let width = width.max(2);
        let uv = ibl::dir_to_equirect_uv(arr(d));
        let jitter = blue_jitter((uv[0] * width as f32) as u32, (uv[1] * (width / 2) as f32) as u32);
        let sigma = e.sigma;
        let ds = RAIN_STEP_M;
        let (l, shadowed) = (self.sun, e.base - BASE_RAG_M * (1.0 + BASE_RAG2) - 100.0);
        let (mut t, mut tau, mut column) = (near + jitter * ds, 0.0f32, 0.0f32);
        while t < far {
            let p = vec3f(d.x * t, height_at(t, d.y), d.z * t);
            let footprint = t * angle;
            // under the base's skins the rain alone (`read`'s there)
            let (density, rain) = match e.rain_only(&self.tiles, p, footprint) {
                Some(rain) => (rain, rain),
                None => {
                    let r = self.shape.read(&self.tiles, p, footprint, Lod::Far);
                    (r.density, r.rain)
                }
            };
            if density > 0.0 {
                let rain = rain * sigma;
                let dtau = (density * sigma - (1.0 - BOW_PASS) * rain) * ds;
                if rain > 0.0 {
                    // `sun_depth`, the storm's shadow under the lowest base from its table
                    let tau_sun = match &self.shade {
                        Some(shade) if p.y < shadowed && l.y >= 0.03 => {
                            let at = p + l * ((e.base + RAIN_SHADE_M - p.y) / l.y);
                            RAIN_SHADOW_TAU * (1.0 - shade.lit_at(at.x, at.z))
                        }
                        _ => self.sun_depth(p, footprint).0,
                    };
                    column += rain * ds * (-(tau + 0.5 * dtau) - tau_sun).exp();
                }
                tau += dtau;
                if (-tau).exp() <= MIN_TRANSMITTANCE {
                    break;
                }
            }
            t += ds;
        }
        column
    }

    /// The sun's depth from `p` and the powder's (the first two steps, over POWDER_REACH_M): under the lowest base, the
    /// storm's shadow read once where the sun's ray meets the base (the GPU's rule: a march's steps through the base's
    /// edge were all in or all out, speckles); else CPU_SUN_STEPS steps growing CPU_SUN_GROWTH times to where the sun's
    /// ray leaves the ceiling, on the turrets alone.
    fn sun_depth(&self, p: Vec3f, footprint_m: f32) -> (f32, f32) {
        let l = self.sun;
        let Shape::Storm(e) = &self.shape;
        if p.y < e.base - BASE_RAG_M * (1.0 + BASE_RAG2) - 100.0 {
            let at = p + l * ((e.base + RAIN_SHADE_M - p.y) / l.y.max(0.03));
            let lit = smoothstep(-RAIN_SHADE_EDGE_M, RAIN_SHADE_EDGE_M, e.envelope_distance(at));
            return (RAIN_SHADOW_TAU * (1.0 - lit), 0.0);
        }
        let exit = ((self.shape.floor_ceiling().1 - p.y) / l.y.max(0.03)).clamp(200.0, 40_000.0);
        let total = (CPU_SUN_GROWTH.powi(CPU_SUN_STEPS as i32) - 1.0) / (CPU_SUN_GROWTH - 1.0);
        let (mut tau, mut local, mut b0, mut first) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        let mut length = exit / total;
        for k in 0..CPU_SUN_STEPS {
            let b1 = b0 + length;
            let x = p + l * (0.5 * (b0 + b1));
            let lod = if k < SUN_SOFT_FROM { Lod::Far } else { Lod::Soft };
            tau += self.shape.read(&self.tiles, x, footprint_m.max(0.25 * length), lod).density * self.shape.sigma() * length;
            if k < 2 {
                (local, first) = (tau, b1);
            }
            b0 = b1;
            length *= CPU_SUN_GROWTH;
        }
        (tau, local * (POWDER_REACH_M / first.max(1.0)).min(4.0))
    }

    /// The depth over `p` the dome's light comes through: a storm's two marches tilted UP_TILT to either side of the
    /// zenith, averaged, read no finer than UP_SOFT_M, at UP_DEPTH of it (the GPU's rule).
    fn up_depth(&self, p: Vec3f, footprint_m: f32) -> f32 {
        match &self.shape {
            Shape::Storm(_) => {
                let rise = (1.0 - UP_TILT * UP_TILT).max(0.0).sqrt();
                let f = footprint_m.max(UP_SOFT_M);
                let east = self.up_march(p, vec3f(UP_TILT, rise, 0.0), f);
                let west = self.up_march(p, vec3f(-UP_TILT, rise, 0.0), f);
                let south = self.up_march(p, vec3f(0.0, rise, UP_TILT), f);
                let north = self.up_march(p, vec3f(0.0, rise, -UP_TILT), f);
                0.25 * (east + west + south + north) * UP_DEPTH
            }
        }
    }

    /// The depth from `p` along the unit `dir`: CPU_UP_STEPS steps doubling from UP_FIRST_M, each read at its middle no
    /// finer than half its length on the cells' smooth profile (the GPU's cb_up).
    fn up_march(&self, p: Vec3f, dir: Vec3f, footprint_m: f32) -> f32 {
        let (mut tau, mut b0, mut length) = (0.0f32, 0.0f32, UP_FIRST_M);
        for _ in 0..CPU_UP_STEPS {
            let b1 = b0 + length;
            let x = p + dir * (0.5 * (b0 + b1));
            tau += self.shape.read(&self.tiles, x, footprint_m.max(0.5 * length), Lod::Soft).density * self.shape.sigma() * length;
            b0 = b1;
            length *= 2.0;
        }
        tau
    }

    /// Its unfaded opacity along `d` (any length), unlit: the march's walk on the light density, stopped at T 0.01.
    pub fn opacity(&self, d: Vec3f) -> f32 {
        let length = d.length();
        if !(length > 0.0) {
            return 0.0;
        }
        let d = d / length;
        if !(d.y > 0.0) {
            return 0.0;
        }
        let Some((near, far)) = self.shape.span(d) else {
            return 0.0;
        };
        let angle = std::f32::consts::TAU / LAW_WIDTH_LEAST as f32;
        let (mut t, mut tau) = (near, 0.0f32);
        while t < far {
            let p = vec3f(d.x * t, height_at(t, d.y), d.z * t);
            let footprint = t * angle;
            let r = self.shape.read(&self.tiles, p, footprint, Lod::Far);
            if !(r.density > 0.0) {
                let fine = footprint.max(8.0);
                t += if r.safe { r.free_m.max(footprint.clamp(8.0, 80.0)) } else { r.free_m.clamp(fine, 6.0 * fine) };
                continue;
            }
            let sig = r.density * self.shape.sigma();
            let ds = (CPU_STEP_PATHS / (r.full * self.shape.sigma())).clamp(footprint.max(8.0), CPU_STEP_MOST_M);
            tau += sig * ds;
            if (-tau).exp() <= MIN_TRANSMITTANCE {
                return 1.0;
            }
            t += ds;
        }
        1.0 - (-tau).exp()
    }

    /// What it hides along `d`, unfaded (the key's cone and the facts read it).
    pub fn cover_toward(&self, d: Vec3f) -> f32 {
        self.opacity(d)
    }

    /// The light pass's stand-in along `d` (design §4.3): its unlit opacity, and the depth that stands for.
    pub fn stand_in(&self, d: Vec3f) -> CoverSample {
        let cover = self.opacity(d);
        CoverSample { cover, tau: -(1.0 - cover.min(0.999)).ln() }
    }

    /// The key of its GPU record for bakes `width` wide, cut at `cuts`, its lobes mixed at `lining`, asked in `mode` (it
    /// marches in the export mode's either way), its texels' directions moved by `jitter` texels.
    pub fn gpu_key(&self, width: usize, cuts: &[f32], lining: f32, mode: GpuMode, jitter: [f32; 2]) -> u64 {
        let bits = f32::from_bits;
        // (whether its rain takes the sun's light by the drops' phase changes what the GPU records)
        let mut fields = vec![bits(self.key as u32), bits((self.key >> 32) as u32), lining, bits(mode as u32), jitter[0], jitter[1], if self.bow.is_some() { 1.0 } else { 0.0 }];
        fields.extend_from_slice(cuts);
        footprint_grid_key(&fields, self.stream, width, GPU_KEY_PART)
    }

    /// What the GPU marches it from at bakes `width` wide, cut at `cuts`, its lobes mixed at `lining`, its texels'
    /// directions moved by `jitter` texels: two parts, each its key alone where `resident` says the GPU holds it, and its
    /// numbers as its own uniforms. The Tiles part is the Cumulus' export-mode one (the map seed's billow, detail and
    /// shape tiles under the same key), so the GPU holds the noise tiles once for every volume of a seed; the Tables part
    /// holds the blue noise's ranks alone. Its only GPU march is the export mode's (its type's export shader), so
    /// an ask in the CPU mode marches it so too. Err where its own uniforms were given a buffer (`into_uniforms`).
    pub fn gpu_upload(&self, width: usize, cuts: &[f32], lining: f32, mode: GpuMode, jitter: [f32; 2], resident: &dyn Fn(u64) -> bool) -> Result<VolumeUpload, String> {
        use crate::cloud_volume_gpu::{index, key_of, part_key, GpuPart, PartKind, UploadBuilder, PASS_SEGMENTS};
        let part = |kind: PartKind, key: u64, build: &dyn Fn(&mut UploadBuilder)| {
            let key = part_key(kind, key);
            if resident(key) {
                return GpuPart { kind, key, data: None };
            }
            let mut b = UploadBuilder::default();
            build(&mut b);
            b.part(kind, key)
        };
        let (noise, billow) = (&self.tiles.noise, &self.tiles.billow);
        let shape_edge = noise.shape.size();
        let tiles = part(PartKind::Tiles, key_of(&[noise.seed() as u64, billow.size() as u64, noise.detail.size() as u64, GpuMode::Export as u64, shape_edge as u64]), &|b| {
            let at = b.vectors(billow.gpu_texels());
            let detail = b.vectors(noise.detail.gpu_texels());
            b.uniform("tiles", [index(at), billow.size() as f32, index(detail), noise.detail.size() as f32]);
            let shape = b.vectors(noise.shape.gpu_texels());
            b.uniform("tiles2", [index(shape), shape_edge as f32, 0.0, 0.0]);
        });
        let ranks = crate::cloud_volume::blue_ranks();
        let content: Vec<u64> = std::iter::once(GPU_KEY_PART.to_bits() as u64).chain(ranks.iter().map(|&r| r as u64)).collect();
        let tables = part(PartKind::Tables, key_of(&content), &|b| {
            let at = b.scalars(ranks.iter().map(|&r| r as f32));
            b.uniform("tables", [0.0, 0.0, 0.0, index(at)]);
        });
        let mut out = UploadBuilder::default();
        crate::volumetric::gpu_layout(&mut out);
        crate::cloud_volume_light::gpu_layout(&mut out);
        let l = self.sun;
        let (base, top) = self.shape.base_top();
        out.uniform("sun_l", [l.x, l.y, l.z, l.y]);
        out.uniform("shell", [base, top, top - base, self.shape.sigma()]);
        out.uniform("k_walk2", [0.1, 5.0, LAW_WIDTH_LEAST as f32, 0.0]);
        self.shape.uniforms(&mut out, self.bow.is_some());
        let cut = |k: usize| cuts.get(k).copied().unwrap_or(f32::INFINITY).min(1.0e30);
        debug_assert!(cuts.len() < PASS_SEGMENTS);
        out.uniform("cuts", [cut(0), cut(1), cut(2), cuts.len().min(PASS_SEGMENTS - 1) as f32]);
        out.uniform("light", [lining, GpuMode::Export as u32 as f32, width as f32, 0.0]);
        out.uniform("jitter", [jitter[0], jitter[1], 0.0, 0.0]);
        let key = self.gpu_key(width, cuts, lining, mode, jitter);
        let uniforms = out.into_uniforms()?;
        Ok(VolumeUpload { genus: self.shape.genus(), key, width, segments: cuts.len() + 1, parts: vec![tiles, tables], uniforms, record: None })
    }
}
