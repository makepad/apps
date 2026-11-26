//! A light's modifier (`LightParams::modifier`): what a real studio light's
//! face looks like up close, generated from the owner's photographed plates
//! (3D Collective, HDR Light Studio, area light maps; measured, nothing
//! copied):
//! a softbox's flash seen through two layers of cloth (2.3 x its median at
//! the centre, half of that half way out, a floor of 0.3), the inner
//! baffle's hot core and folds inside the box's lit silver walls, the open
//! box's bare tube (0.77 of its light) in its black back with crumpled
//! silver walls; an octabox's
//! eight seams; a white umbrella's ring round the strobe head's silhouette,
//! its stand, ribs and folds; a silver umbrella's hot zone a hundred times
//! its outer third, streaked along its panels, crumpled and glinting; a
//! beauty dish's dark plate, bright lip and hammered bowl, or its sock; an
//! LED ring's rows of diodes round a hole; a fresnel's lamp image in its
//! stepped lens inside a dark barrel; a bare head's frosted dome with its
//! tube ring; an LED panel's lattice of hot points or, diffused, its soft
//! panel; a gradient's scrim lit from behind; a street lamp's luminaire in its halo
//! on its pole (from the GSG night scenes).
//!
//! The modifier is a pattern on the face, as the wear is (imperfections.rs):
//! it multiplies the light's emission at each point of its face (`factor`,
//! about 0.04 where the housing or a rib hides the face, near black in a
//! fresnel's barrel, about 1 over the face as the plates read, far above at
//! a hotspot). It is not
//! renormalised as a whole: a housing that hides a tenth of the face takes a
//! tenth of its light; the faces are laid out so their mean over the face
//! is about 1 (an open box's bare tube holds most of it), so a light's EV
//! keeps its meaning across kinds. A hot core smaller than a key's strata
//! (the tube) is kept apart (`core_at`, `core_mass`) so the key's share
//! takes it whole. Its outline is the light's own shape
//! (`ModifierKind::shape`, which "+ Light" sets with it); the pattern is laid
//! out on the light's box, its edge where the box's is, so a modifier on
//! another shape still reads.
//!
//! What the five knobs mean, per kind (`ModifierParams`):
//! - `rim`: the dark housing round the face, a share of the half size;
//! - `baffle`: a softbox's diffusion, from both diffusers (0) through the
//!   inner baffle alone (0.7) to the open box (1); an umbrella's hot zone (0 a
//!   flat parabolic, 0.6 a white bounce, 1 a deep silver); a beauty dish's
//!   sock, an LED's diffuser, a fresnel's lamp image's sharpness, a
//!   gradient's lamp's beam, a street lamp's halo (the night's haze);
//! - `ribs`: an umbrella's or octabox's ribs, a ring's rows of diodes (ten
//!   a row), a fresnel's lens steps (fourteen), a panel's diodes across; a gradient's
//!   lamp a strip at 0, round at 1, that many strips above; a street lamp has a pole above 0;
//! - `plate`: a beauty dish's centre plate, an umbrella's head (its radius is
//!   half of it), a ring's hole, a bare head's dome, a street lamp's
//!   luminaire, all shares of the radius; how far below the middle a
//!   gradient's round lamp stands;
//! - `diffusion` and `across`: a gradient's lamp's distance behind its scrim
//!   and its place across it (`Scrim::new`).
//!
//! Positions are face coordinates (tangent units, x to the viewer's right, y
//! up); `u = q / half` puts the box's edge at 1. Per call: a few exponentials
//! and up to three octaves of value noise, or nine cells of facets, under a
//! microsecond.
use makepad_draw::*;

use super::noise::{fbm2, fbm3, hash01, hash_u32, value2};
use crate::sky::luminance;
use super::open_box::OpenBox;
use super::patterns::{broad_folds, fan_folds, fbm, patches};
use super::reflector::Reflector;
use super::params::{ModifierKind, ModifierParams, SCRIM_CREASES_DEFAULT, SCRIM_FILL_ACROSS_DEFAULT};
use super::silver_deep::DeepSilver;

/// The width a rib, a seam or a ring is drawn at, in box units, and the soft
/// edge of every part (the bake point-samples the face).
const LINE: f32 = 0.018;
/// An open box's bare tube: a crisp disc, `TUBE_RADIUS_BOX` of the shorter
/// half size across in a softbox or strip and `TUBE_RADIUS_OCTA` in an
/// octabox (as the plates show it), never under `TUBE_MIN_TAN` (0.3
/// degrees, so a 1K bake still sums it right), holding `TUBE_SHARE_BOX` of
/// a softbox's or strip's light and `TUBE_SHARE_OCTA` of an octabox's (the
/// plates' tubes hold 0.77 and 0.87 of the face's light; an octagon with its
/// corners on the box fills `OCTAGON_SHARE` of it), with no halo round it.
const TUBE_RADIUS_BOX: f32 = 0.06;
const TUBE_RADIUS_OCTA: f32 = 0.045;
const TUBE_MIN_TAN: f32 = 0.0052;
const TUBE_SHARE_BOX: f32 = 0.77;
const TUBE_SHARE_OCTA: f32 = 0.87;
const OCTAGON_SHARE: f32 = std::f32::consts::FRAC_1_SQRT_2;
/// The area of the tube's crisp disc, 1 - smooth(0.85 R, 1.15 R, r), over pi R².
const TUBE_DISC_AREA: f32 = 1.0045;
/// How far the inner baffle's cloth sits in from a softbox's side, in short
/// half sizes (RLS011); the top and bottom walls show twice as deep.
pub(crate) const BAFFLE_SIDE: f32 = 0.19;
/// A strip's inner cloth sits deeper (RLS022): 0.37 short half sizes in from
/// its sides and 1.4 from its ends; its side walls lit as its cloth is
/// beside them, at `STRIP_SIDE_WALLS` of it, its ends' walls dark; its
/// cloth's tension fans at `STRIP_INNER_FANS` of a softbox's front's, its
/// seams' ridges `STRIP_SEAM` high over `STRIP_SEAM_WIDTH` (short half
/// sizes: about a millimetre over a few).
pub(crate) const STRIP_BAFFLE_SIDE: f32 = 0.37;
pub(crate) const STRIP_BAFFLE_END: f32 = 1.4;
const STRIP_RELIEF: f32 = 0.5;
const STRIP_SIDE_WALLS: f32 = 0.6;
const STRIP_END_WALLS: f32 = 0.13;
const STRIP_INNER_FANS: f32 = 0.2;
const STRIP_SEAMS: [f32; 2] = [0.48, 0.71];
const STRIP_SEAM: f32 = 0.0014;
const STRIP_SEAM_WIDTH: f32 = 0.008;
/// A fresnel's lens edge and the pitch of the wire guard over it, in box
/// units (the area map's wires 0.28 of the lens radius apart), its barrel's
/// level (the area map: 0.005 of the face's mean), and the lens's middle
/// disc, a share of the lens radius (the Moles' first step at 0.24).
pub(crate) const LENS_R: f32 = 0.69;
const MESH: f32 = 0.19;
const BARREL: f32 = 0.004;
pub(crate) const LENS_DISC: f32 = 0.24;
pub(crate) const EDGE: f32 = 0.012;
/// A bare head's ring of the tube (`plate`, a share of the radius) under
/// which its 3D model stands in a reflector (light_model_head.rs; the face
/// brings the reflector in from 0.42 down to 0.3).
pub(crate) const BULB_REFLECTOR_BELOW: f32 = 0.36;
/// A bare head's flash tube (`bare_head`): its glass's radius, the half
/// opening at its top (radians), and the height of its centre line over the
/// plate the face lights the plate from, in the tube ring's units (`D`).
pub(crate) const TUBE_GLASS: f32 = 0.17;
pub(crate) const TUBE_GAP: f32 = 0.36;
const TUBE_HEIGHT: f32 = 0.17;
/// A bare head's modelling lamp's dome and a reflector head's big frosted
/// globe, their radii in D units (`bare_head`, `bulb_with`).
pub(crate) const DOME_R: f32 = 0.66;
pub(crate) const GLOBE_R: f32 = 1.42;
/// A clear-glass reflector spot bulb (`Modifier::spot_bulb_raw`), box
/// units: its rim's radius; where its middle stands (on the rim's: the
/// bulb is round, the area map only sees it a little turned); its
/// capsule's front's radius; its C of
/// filament's middle radius and half width, and the half opening of the C.
pub(crate) const SPOT_RIM: f32 = 0.975;
pub(crate) const SPOT_MIDDLE: Vec2f = Vec2f { x: 0.0, y: 0.0 };
pub(crate) const SPOT_CAP_R: f32 = 0.215;
pub(crate) const SPOT_COIL_R: f32 = 0.41;
pub(crate) const SPOT_COIL_W: f32 = 0.045;
pub(crate) const SPOT_COIL_GAP: f32 = 0.43;

/// What a bare head's 3D model (light_model_head.rs `bulb`) stands in front
/// of its face as glass, so its bare face (`Modifier::factor_bare`) leaves it
/// out: its flash tube, its modelling lamp's dome, a reflector head's big
/// frosted globe.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BulbGlass {
    pub tube: bool,
    pub dome: bool,
    pub globe: bool,
}

/// What a bare head of tube ring `plate` with its modelling lamp lit by
/// `lit` (`baffle`) has as glass: in a reflector, lit by half or more, the
/// big globe over everything; else the tube, and the dome while the lamp is
/// in at all (off, RLS007 shows its empty socket).
pub(crate) fn bulb_glass(plate: f32, lit: f32) -> BulbGlass {
    let globe = plate.max(0.15) < BULB_REFLECTOR_BELOW && lit >= 0.5;
    BulbGlass { tube: !globe, dome: !globe && lit > 0.0, globe }
}
/// The housing and a rib are not quite black: black fabric and metal catch some light.
const HOUSING: f32 = 0.04;
/// A reflector panel's binding round its outline, a share of the shorter
/// half size (RLS038-042: 0.6-0.9 % of the width, dark).
const REFLECTOR_BINDING: f32 = 0.016;
/// A diffused LED panel's frame round its cloth, a thin band darker than a
/// housing (RLS034's reads near black in a grab where a 0.04 band read
/// grey), and its corners past it: the panel's black, the studio's in the plate.
const FRAME: f32 = 0.015;
/// A diffused LED panel's diodes seen through its cloth (RLS034): a square
/// `LED_CORE_HALF` of the shorter half size, adding `LED_CORE` of the halo's
/// middle, its edge soft over `LED_CORE_SOFT` (shorter half sizes).
const LED_CORE_HALF: f32 = 0.42;
/// A bare diode's cell's mean (`led_bare`, lit 1), a 2000 x 2000 midpoint sum.
const LED_BARE_MEAN: f32 = 0.081341;
/// How far a panel's columns of diodes and its diodes one by one stray
/// from their mean, either way (RLS033: a column a fifth over its
/// neighbours; RLS031's diodes a few percent apart).
const LED_COLUMN_SPREAD: f32 = 0.1;
const LED_DIODE_SPREAD: f32 = 0.06;
/// A diffused diode's glow (`led_glow`): the core's share of its light,
/// and the diffuser's floor, a share of the face's mean.
const LED_GLOW_CORE: f32 = 0.25;
const LED_GLOW_FLOOR: f32 = 0.25;
/// A nude COB's dies' share of its field (`nude_led_raw`): 0.72 x 0.4 of
/// each cell, their smooth edges taken in.
const NUDE_DIE_SHARE: f32 = 0.288;
/// A single-chip head's dies (`single_chip_raw`, Single_Chip_LED: about a hundred), chip radii: each a block
/// of these half sizes, the rows `SINGLE_PITCH.y` apart and the dies `SINGLE_PITCH.x` along them, every other
/// row half a die along, the lattice turned `SINGLE_TURN` (radians); the phosphor between them lit
/// `SINGLE_GAP` of a die; the dies' share of their cells. The holder's screw ears stand `SINGLE_EAR_AT` chip
/// radii out at `SINGLE_EAR_ANGLE` (radians, counter-clockwise from the right) and across from it.
const SINGLE_DIE: Vec2f = Vec2f { x: 0.068, y: 0.035 };
const SINGLE_PITCH: Vec2f = Vec2f { x: 0.21, y: 0.16 };
const SINGLE_TURN: f32 = 0.2;
const SINGLE_GAP: f32 = 0.22;
const SINGLE_DIE_SHARE: f32 = 0.283;
pub(crate) const SINGLE_EAR_AT: f32 = 1.79;
pub(crate) const SINGLE_EAR_ANGLE: f32 = 2.57;
/// A ring light's lips inside and out, a share of the ring's width, and
/// how far apart its diodes sit along a row, in row pitches (RLS035, RLS036).
const RING_LIP: f32 = 0.125;
const RING_ALONG: f32 = 1.8;
/// A ring's widest row pitch, in radii: a diode keeps its size, so rows too
/// few to fill a wide ring sit in its middle on the lit plastic (the area
/// map's LED_Ring: one row of 36 at 0.87 of the radius, 0.15 apart, in a ring
/// from 0.76).
const RING_PITCH: f32 = 0.085;
/// A thick diffuser's glow across the ring from each row of diodes, as a
/// share of the ring's width (`Modifier::ring_diffused`).
const RING_DIFFUSER_SPREAD: f32 = 0.4;
/// A thick diffuser's diode glow's peak over the plastic at its row, at
/// `diffusion` 0.2 (LED_Ring: at least 2.4 x the gaps' level, clipped).
const RING_DIFFUSER_GLOW: f32 = 1.6;
/// A closed cloth face's diffusion (`Modifier::spot`): at most this,
/// and the hotspot's share there.
pub const DIFFUSION_MAX: f32 = 0.8;
const DIFFUSION_SPOT: f32 = 0.2;
const DIFFUSION_EVEN: f32 = 0.35;
const LED_CORE: f32 = 0.34;
const LED_CORE_SOFT: f32 = 0.09;
const CORNER: f32 = 0.004;
/// A diffuser over an umbrella (`Modifier::cover`): how far its hem bows in
/// between two rib tips (RLS026's, a share of the radius), how its folds
/// stand (`cover_height`, box units), and how far behind it the bowl's lit
/// middle sits.
const COVER_SAG: f32 = 0.035;
const COVER_FOLDS: f32 = 0.002;
const COVER_GATHER: f32 = 0.08;
const COVER_GATHER_ALONG: f32 = 7.0;
const COVER_GATHER_IN: f32 = 0.3;
const COVER_GATHER_ACROSS: f32 = 2.0;
const COVER_DEPTH: f32 = 1.2;
/// A white umbrella's cover glows round the head (RLS026): this much over
/// its bounce's hot zone, falling by 1/e this far from the flash.
const COVER_GLOW: f32 = 4.0;
const COVER_GLOW_R: f32 = 0.25;
/// An umbrella's stand: half its width in box units (3-4 % of the radius in the plates).
const STAND_HALF_WIDTH: f32 = 0.017;
/// What a white umbrella's translucent cloth lets through to its back, over
/// what its front gives back (`Modifier::back`): Lastolite's white
/// translucent nylon passes about as much as it reflects.
const TRANSLUCENT_BACK: f32 = 0.8;

/// A fluorescent bank (the ArtStation vol 4 fluorescent plates, the Kino
/// Bank area map): a tube's lit length over its diameter (T8 and T12 tubes;
/// the plates' 29 to 44), so the tubes' size follows
/// the bank's length, and the widest a tube gets as a share of its pitch
/// (pnl_lamps_01's tubes packed side by side, 0.85).
const FLUO_LENGTH_PER_DIAMETER: f32 = 34.0;
const FLUO_FILL_MAX: f32 = 0.85;
/// How far in front of its reflector a tube's axis stands, in tube radii.
const FLUO_STANDOFF: f32 = 1.8;
/// The electrode end: its length in diameters and its glass's light at the
/// cap as a share of the tube's (pnl_lamps_01 and the Kino Bank: the glass
/// whiter and dimmer over the last diameter and a half, half the tube's at the cap).
const FLUO_ELECTRODE: f32 = 1.5;
const FLUO_ELECTRODE_LEVEL: f32 = 0.45;
/// The metal end cap and the socket past it, in diameters, and their light
/// as a share of the tube's (the cap a lit ring, the socket near black).
const FLUO_CAP: f32 = 0.35;
const FLUO_CAP_LEVEL: f32 = 0.06;
const FLUO_SOCKET: f32 = 0.7;
const FLUO_SOCKET_LEVEL: f32 = 0.008;
/// The clips that hold each tube, this share of its half length out from
/// the middle (the Kino Bank's at 0.76), dark.
const FLUO_CLIP_AT: f32 = 0.77;
const FLUO_CLIP_LEVEL: f32 = 0.012;
/// The reflector: a mirror's reflectance (polished aluminium, foil), and the
/// light it scatters as a share of the tube's: crumpled foil's micro
/// creases scatter a lot (fluorescent_big_lamps: its foil about a third
/// of the tubes), a polished trough little.
const FLUO_MIRROR: f32 = 0.85;
const FOIL_DULL: f32 = 0.2;
/// What the reflector mirrors where it misses the tubes: the lit reflector
/// and frame round it, as a share of a tube's light.
const FLUO_AROUND: f32 = 0.15;
const TROUGH_DULL: f32 = 0.08;
/// The housing round the bay and the dark it shows, as a share of a tube's light.
const FLUO_HOUSING: f32 = 0.02;
/// Crumpled foil (`foil_slope`, short half sizes): the fine creases one to
/// a cell of `FOIL_CELL`, each bending the foil by `FOIL_BEND` (a slope),
/// the flat facets `FOIL_FACETS` to a half size tilted by up to
/// `FOIL_TILT`, the crinkle in patches. Kitchen foil pressed flat and
/// opened again tilts its facets 10-20 degrees (fluorescent_big_lamps).
const FOIL_CELL: f32 = 0.012;
const FOIL_BEND: f32 = 0.35;
const FOIL_FACETS: f32 = 45.0;
const FOIL_TILT: f32 = 0.25;
const FOIL_CRINKLE: f32 = 40.0;
const FOIL_CRINKLE_AMP: f32 = 0.0015;
/// The tube radius `foil_slope`'s sizes are given at (fluorescent_big_lamps'
/// sixteen tubes, in short half sizes).
const FOIL_RADIUS: f32 = 0.03;
/// The foil is pulled taut along the bank: its folds this much longer along
/// the tubes than across (fluorescent_big_lamps).
const FOIL_ALONG: f32 = 0.5;
/// How wide a cone a reflector blurs a tube's image over, as a slope: the
/// crumpled foil's, a polished trough's.
const FOIL_BLUR: f32 = 0.6;
const TROUGH_BLUR: f32 = 0.03;
/// How far a polished trough reaches each side of its tube, in tube radii
/// (the Kino Bank's and pnl_lamps_03's 2.1 to 2.7).
const TROUGH_REACH: f32 = 3.0;
/// The cloth over a clothed bank (fluorescent_big_clothered): its depth in
/// front of the tubes at `baffle` 1, in short half sizes.
const FLUO_CLOTH_DEPTH: f32 = 0.35;
/// The cloth is tied to the frame all round (fluorescent_big_clothered's
/// eyelets): a tie every this many short half sizes, each pulling a fan
/// this strong (a softbox's corner rod pulls 1).
const FLUO_CLOTH_TIES: f32 = 0.33;
const FLUO_CLOTH_FANS: f32 = 0.25;
/// A fluorescent bank's cover in front of its tubes (`diffusion`, `Modifier::fluo_cover`): up to
/// `FLUO_LOUVRE_TOP` a louvre, `FLUO_LOUVRE_CELLS` x `diffusion` cells across the bay (0.04: a troffer's four,
/// pnl_lamps_04; 0.4: an egg-crate's forty, pnl_squared_01 and 02), past it a prismatic cover, clean up to
/// `FLUO_PRISM_CLEAN` and its dirt rising to 0.8 (fluorescent_dirty).
const FLUO_LOUVRE_TOP: f32 = 0.5;
const FLUO_LOUVRE_CELLS: f32 = 100.0;
const FLUO_PRISM_CLEAN: f32 = 0.53;
/// A louvre's cells, square over the bay; their front edges lit `FLUO_LOUVRE_EDGE_LEVEL` of a tube's light (the
/// room's light on them), softened over `FLUO_LOUVRE_SOFT` of a cell. A louvre of `FLUO_LOUVRE_SPECULAR` cells
/// across or fewer is a troffer's specular aluminium (pnl_lamps_04: the tubes mirrored in its walls); more, a
/// moulded plastic egg-crate (pnl_squared_01: grey where its walls are seen at a slant).
const FLUO_LOUVRE_EDGE_LEVEL: f32 = 0.03;
const FLUO_LOUVRE_SOFT: f32 = 0.008;
const FLUO_LOUVRE_SPECULAR: f32 = 8.0;
/// An egg-crate's cells are cubes (`FLUO_LOUVRE_DEPTH` deep over wide), its front edges `FLUO_LOUVRE_EDGE` of a
/// cell thick, its matte walls lit `FLUO_LOUVRE_WALL` of the light a sheet `FLUO_LOUVRE_WALL_REACH` in front of
/// the tubes would get (short half sizes): a wall sees the tubes behind it all along its depth.
const FLUO_LOUVRE_DEPTH: f32 = 1.0;
const FLUO_LOUVRE_EDGE: f32 = 0.14;
const FLUO_LOUVRE_WALL: f32 = 0.5;
const FLUO_LOUVRE_WALL_REACH: f32 = 0.25;
/// A troffer's louvre (pnl_lamps_04): blades `FLUO_TROFFER_EDGE` of a cell thick at the front, the cell
/// `FLUO_TROFFER_DEPTH` deep; its blades along the tubes lean in `FLUO_TROFFER_TAPER` of the cell by the back,
/// its cross blades `FLUO_TROFFER_CROSS_TAPER`: head-on each wall's face shows as a strip along the cell's side,
/// the tubes mirrored in it, `FLUO_LOUVRE_MIRROR` returned at each bounce.
const FLUO_TROFFER_EDGE: f32 = 0.012;
const FLUO_TROFFER_DEPTH: f32 = 0.45;
const FLUO_TROFFER_TAPER: f32 = 0.06;
const FLUO_TROFFER_CROSS_TAPER: f32 = 0.06;
const FLUO_LOUVRE_MIRROR: f32 = 0.85;
/// A prismatic cover (fluorescent_dirty, a vapour-tight fitting's moulded bowl): `FLUO_PRISM_GAP` in front of
/// the tubes (short half sizes), its ribs running along the tubes `FLUO_PRISM_PITCH` apart, a moulded wave whose
/// slope bends the ray through it by up to about `FLUO_PRISM_BEND`, so each rib shows the tubes smeared across
/// it; `FLUO_PRISM_HAZE` of what passes is scattered (the milky moulding), and it passes `FLUO_PRISM_PASS`, less
/// where the bowl turns toward edge-on at its long sides (`FLUO_PRISM_TURN`).
const FLUO_PRISM_GAP: f32 = 0.45;
const FLUO_PRISM_PITCH: f32 = 0.028;
const FLUO_PRISM_BEND: f32 = 0.35;
const FLUO_PRISM_HAZE: f32 = 0.25;
const FLUO_PRISM_PASS: f32 = 0.85;
const FLUO_PRISM_TURN: f32 = 0.55;
/// A dirty cover's dust (`Modifier::fluo_dirt_depth`): its film's optical depth through green (`FLUO_DUST`, at
/// dirt 1) in patches `FLUO_DUST_PATCH` m across, `FLUO_DUST_EDGE_MORE` times more within `FLUO_DUST_EDGE` m of the
/// edges; grains (`FLUO_GRAIN`) and flecks (`FLUO_FLECK`), one to a cell at most; on a cover whose long half is
/// `FLUO_DIRT_LONG` m, seen at `FLUO_DIRT_FOOT` m at the finest. Dust is grey-brown: it takes blue
/// `FLUO_DIRT_ABSORB` times more than green, red a little less.
const FLUO_DIRT_LONG: f32 = 0.6;
const FLUO_DIRT_FOOT: f32 = 0.0006;
const FLUO_DIRT_ABSORB: [f32; 3] = [0.85, 1.0, 1.35];
const FLUO_DUST: f32 = 0.6;
const FLUO_DUST_PATCH: f32 = 0.18;
const FLUO_DUST_EDGE: f32 = 0.035;
const FLUO_DUST_EDGE_MORE: f32 = 1.5;
const FLUO_GRAIN_CELL: f32 = 0.0018;
const FLUO_GRAIN: f32 = 1.6;
const FLUO_FLECK_CELL: f32 = 0.02;
const FLUO_FLECK: f32 = 2.5;

/// What a dirty cover's dust of optical depth `depth` (through green) lets through, per channel
/// (`FLUO_DIRT_ABSORB`).
fn fluo_dirt_pass(depth: f32) -> Vec3f {
    let a = FLUO_DIRT_ABSORB;
    vec3f((-depth * a[0]).exp(), (-depth * a[1]).exp(), (-depth * a[2]).exp())
}

/// What covers a fluorescent bank's tubes (`Modifier::fluo_cover`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum FluoCover {
    Open,
    /// A louvre of this many cells across the bay.
    Louvre(f32),
    /// A prismatic cover, its dirt 0..=1.
    Prism(f32),
}

/// A fluorescent bank's layout (`Modifier::fluo_layout`), short half sizes.
struct FluoLayout {
    /// The tubes run along x (the face is wider than tall), else along y.
    wide: bool,
    /// The face's half size along the tubes.
    long: f32,
    /// The bay's half sizes inside the housing, across and along.
    bay: f32,
    bay_long: f32,
    /// The tubes, and their pitch across.
    n: f32,
    pitch: f32,
    /// A tube's lit half length and its radius.
    lit: f32,
    radius: f32,
}

#[derive(Clone, Debug)]
pub struct Modifier {
    kind: ModifierKind,
    half: Vec2f,
    rim: f32,
    baffle: f32,
    ribs: f32,
    plate: f32,
    /// A closed cloth face's diffusion (`ModifierParams::diffusion`, `spot`).
    diffusion: f32,
    /// What the folds and glints hang on: fixed by the kind and the size, so
    /// a light draws the same every bake and two sizes draw differently.
    seed: u32,
    /// What the face's pattern is divided by to sit at mean 1 (`face_norm`):
    /// a softbox's diffused face, an umbrella's hot zone, a bare head's or a
    /// beauty dish's face (`disc_mean`), a fresnel's lens over what its
    /// barrel leaves, a diffused LED panel's face (`led_diffuser`), a
    /// gradient's lamp through its scrim (`Scrim::mean`), a ring light's face
    /// (`ring_mean`), a nude LED's (`nude_led_raw`); 1 for the rest.
    norm: f32,
    /// An open box's silver lining (open_box.rs), when `baffle` opens it,
    /// and an octabox's with its inner baffle (its folds only).
    open: Option<OpenBox>,
    /// A deep silver umbrella's face (silver_deep.rs), when `baffle` is 0.75
    /// or more: the flash's clipped core breaking into folded lobes, with
    /// its own strobe head and stand.
    deep: Option<DeepSilver>,
    /// A shallower silver umbrella's fabric (silver_deep.rs: the deep one's
    /// gores, ribs, rib folds and foil without its reflection), under `baffle` 0.75.
    shallow: Option<DeepSilver>,
    /// A reflector panel's cloth (reflector.rs).
    reflector: Option<Reflector>,
    /// A gradient's lamp behind its scrim (`Scrim`).
    scrim: Option<Scrim>,
    /// The colour the face's light takes on average (`tint_mean`), None where it keeps the light's own.
    tint_mean: Option<Vec3f>,
}

/// A gradient panel's lamp: a scrim lit from behind by one round lamp or by
/// strip lamps standing up the box (the 3D Collective gradients, RLS043-054,
/// all read so within 1-5 %), each aimed at the scrim's middle. Box units,
/// the scrim at depth 0: `at` its place in the scrim's plane (a strip's x
/// alone), `h` how far behind the scrim it stands, `beam` the exponent of
/// its beam's cos^beam fall off its axis. A second lamp of the same kind,
/// depth and beam (`fill`, at `fill_at`) and a round lamp's aim (`aim`) come
/// from the knobs; the scrim's creases (`creases`, `SCRIM_BANDS`) are lit by
/// the lamps as seen from `CREASE_DEPTH` behind it.
#[derive(Clone, Debug)]
struct Scrim {
    /// 0 a round lamp, else that many strips.
    strips: u32,
    at: Vec2f,
    h: f32,
    beam: f32,
    /// The second lamp's light as a share of the first's (0 none) and its place.
    fill: f32,
    fill_at: Vec2f,
    /// Where a round lamp's beam is aimed in the scrim's plane, None its middle.
    aim: Option<Vec2f>,
    /// The creases' depth (1 the plates' scrim), and the box's half sizes over
    /// its shorter one (the creases keep their size in the cloth).
    creases: f32,
    aspect: Vec2f,
}

/// The scrim's creases (RLS043-049, 053, 054 share one scrim): a few bands
/// of long smooth folds running in from the frame toward a pinch near the
/// middle, where the cloth was caught and let go. By the owner's fabric
/// recipe each band is a tension fan's folds, parallel along the pull
/// between the pinch and the frame (in its own frame: along, across), soft
/// folds (the plates' are round, no pinched ridge), bending as the pull
/// turns, lit through the height's slope, never painted; the frame keeps the
/// cloth taut within `SCRIM_TAUT` of it. Each band: where it starts (short
/// half sizes from the middle, y up), its way (radians from x), its bend
/// (its folds turn off that way by bend x along squared), its half width
/// across, its height, the folds' phase, how far it runs and over how much
/// of that it fades out. The folds are `SCRIM_FOLD` apart. Fitted to the
/// plates' own scrim: its height recovered from the seven creased plates,
/// each lit by its own lamp (correlation 0.73 with that height, 0.64-0.79
/// with the plates' texture; the plates' folds wander and branch more than
/// seven even bands can), the heights doubled so `creases` 1 is the plates'
/// typical depth.
const SCRIM_FOLD: f32 = 0.1481;
const SCRIM_TAUT: f32 = 0.06;
const SCRIM_BANDS: [[f32; 9]; 7] = [
    [-0.1567, 0.3287, 1.3247, 0.1665, 0.2000, 0.001286, -0.5673, 0.6727, 0.1223],
    [0.0399, -0.7500, 1.4846, -0.3983, 0.1107, 0.002054, 3.5553, 0.9390, 0.0819],
    [-0.8798, -0.3913, 0.2651, 0.2972, 0.1406, 0.001820, 3.3810, 0.5245, 0.3051],
    [0.9249, -0.0792, 3.1348, 0.1037, 0.0782, 0.001495, 4.0744, 0.5112, 0.2893],
    [0.9515, 0.9138, -2.6215, 0.0996, 0.1091, 0.000720, 1.4323, 1.1593, 0.1267],
    [-0.2830, 0.1582, 3.1492, -0.3653, 0.0400, 0.000394, 6.1562, 0.4694, 0.0910],
    [0.0675, -0.3500, 0.7500, 0.2212, 0.1346, 0.001431, 4.5740, 0.7402, 0.0956],
];
/// How far behind the scrim its lamps are seen from when they light its
/// creases, in half sizes: a lamp far off to one side lights a crease's
/// slope facing it at most as the slope (the sine of its incidence).
const CREASE_DEPTH: f32 = 1.0;

impl Scrim {
    /// The lamp of a gradient's knobs (`ModifierParams`): `ribs` 0 one strip,
    /// at `across`; 1 a round lamp at `across`, `plate` below the middle; 2
    /// and more that many strips spread evenly over the box. `across` -1..1
    /// runs from far off the left edge through the left edge (-0.5) and the
    /// middle (0) to far off the right, `plate` 0..0.8 from the middle to far
    /// below (`place`); unset, a strip stands off the left edge (-0.7, a
    /// ramp from the left) and a round lamp in the middle. `diffusion` 0..0.8
    /// stands it from just behind the scrim (0.01 of the half size, its spot
    /// a point) to 1.6 half sizes back (a soft glow), unset 0.6 (0.9 back);
    /// `baffle` 0..1 narrows its beam from a bare lamp (cos^0) to a tight
    /// spot (cos^40), which draws an off-centre lamp's spot out into a tail.
    /// `fill` adds a second lamp at `fill_across` (to one strip or one round
    /// lamp), `aim` turns a round lamp's beam up or down the middle line
    /// (`place`), `creases` deepens the scrim's folds. `half` the face's half
    /// sizes.
    fn new(p: &ModifierParams, half: Vec2f) -> Scrim {
        let (ribs, across, plate) = (p.ribs, p.across, p.plate);
        let place = |a: f32| a / (1.0 - 0.9 * a.abs().min(1.0));
        let x = place(across.unwrap_or(if ribs == 0 { -0.7 } else { 0.0 }));
        let y = if ribs == 1 { -place(plate / 0.8) } else { 0.0 };
        let d = p.diffusion.unwrap_or(0.6);
        let fill = if ribs <= 1 { p.fill.unwrap_or(0.0) } else { 0.0 };
        let fill_at = vec2f(place(p.fill_across.unwrap_or(SCRIM_FILL_ACROSS_DEFAULT)), y);
        let aim = if ribs == 1 { p.aim.map(|a| vec2f(0.0, place(a))) } else { None };
        let short = half.x.min(half.y).max(1.0e-6);
        Scrim {
            strips: if ribs == 1 { 0 } else { ribs.max(1) },
            at: vec2f(x, y),
            h: 0.01 + 2.5 * d * d,
            beam: 40.0 * p.baffle * p.baffle,
            fill,
            fill_at,
            aim,
            creases: p.creases.unwrap_or(SCRIM_CREASES_DEFAULT),
            aspect: vec2f(half.x / short, half.y / short),
        }
    }

    /// One round lamp at `a` (box units): its beam's cos^beam (toward its aim,
    /// the middle unset) times h / r^3 (cos^3 / h^2 under it).
    fn round(&self, u: Vec2f, a: Vec2f) -> f32 {
        let h2 = self.h * self.h;
        let v = u - a;
        let r2 = v.x * v.x + v.y * v.y + h2;
        let beam = |c: f32, len: f32| if self.beam > 0.0 { (c / len).max(0.0).powf(self.beam) } else { 1.0 };
        let lit = match self.aim {
            None => beam(-v.x * a.x - v.y * a.y + h2, (r2 * (a.x * a.x + a.y * a.y + h2)).sqrt()),
            Some(aim) => {
                let axis = aim - a;
                beam(v.x * axis.x + v.y * axis.y + h2, (r2 * (axis.x * axis.x + axis.y * axis.y + h2)).sqrt())
            }
        };
        lit * self.h / (r2 * r2.sqrt())
    }

    /// One strip lamp standing at x (box units): its beam's cos^beam times
    /// h / rho^2 (cos^2 / h).
    fn strip(&self, u: Vec2f, x: f32) -> f32 {
        let h2 = self.h * self.h;
        let beam = |c: f32, len: f32| if self.beam > 0.0 { (c / len).max(0.0).powf(self.beam) } else { 1.0 };
        let v = u.x - x;
        let rho2 = v * v + h2;
        beam(-v * x + h2, (rho2 * (x * x + h2)).sqrt()) * self.h / rho2
    }

    /// What the scrim passes at `u` (box units): the lamp's irradiance, its
    /// beam's cos^beam times h / r^3 for a round lamp (cos^3 / h^2 under it)
    /// and h / rho^2 for a strip (cos^2 / h), and the second lamp's share.
    /// Not normalised, no creases.
    fn at(&self, u: Vec2f) -> f32 {
        if self.strips == 0 {
            let f = self.round(u, self.at);
            return if self.fill > 0.0 { f + self.fill * self.round(u, self.fill_at) } else { f };
        }
        if self.strips == 1 {
            let f = self.strip(u, self.at.x);
            return if self.fill > 0.0 { f + self.fill * self.strip(u, self.fill_at.x) } else { f };
        }
        let n = self.strips as f32;
        (0..self.strips).map(|i| self.strip(u, -1.0 + (2 * i + 1) as f32 / n)).sum()
    }

    /// The way the lamps' light comes in at `u`, in the scrim's plane: the
    /// in-plane part of the unit vector toward each lamp seen `CREASE_DEPTH`
    /// behind the scrim (a strip's nearest point), weighted by its light.
    fn light_dir(&self, u: Vec2f) -> Vec2f {
        let toward = |x: f32, y: f32| {
            let v = vec2f(x - u.x, y - u.y);
            v / (v.x * v.x + v.y * v.y + CREASE_DEPTH * CREASE_DEPTH).sqrt()
        };
        if self.strips == 0 {
            let main = toward(self.at.x, self.at.y);
            if self.fill <= 0.0 {
                return main;
            }
            let (a, b) = (self.round(u, self.at), self.fill * self.round(u, self.fill_at));
            return (main * a + toward(self.fill_at.x, self.fill_at.y) * b) / (a + b).max(1.0e-12);
        }
        let mut sum = vec2f(0.0, 0.0);
        let mut weight = 0.0f32;
        let mut add = |x: f32, w: f32| {
            sum += toward(x, u.y) * w;
            weight += w;
        };
        if self.strips == 1 {
            add(self.at.x, self.strip(u, self.at.x));
            if self.fill > 0.0 {
                add(self.fill_at.x, self.fill * self.strip(u, self.fill_at.x));
            }
        } else {
            let n = self.strips as f32;
            for i in 0..self.strips {
                let x = -1.0 + (2 * i + 1) as f32 / n;
                add(x, self.strip(u, x));
            }
        }
        sum / weight.max(1.0e-12)
    }

    /// The creases' height at `p` (short half sizes from the middle), 0 on a
    /// smooth scrim (`SCRIM_BANDS`).
    fn crease_height(&self, p: Vec2f) -> f32 {
        let frame = (self.aspect.x - p.x.abs()).min(self.aspect.y - p.y.abs());
        let taut = smooth(0.0, SCRIM_TAUT, frame);
        if taut <= 0.0 {
            return 0.0;
        }
        let mut h = 0.0f32;
        for [x, y, angle, bend, width, height, phase, length, fade] in SCRIM_BANDS {
            let (sa, ca) = angle.sin_cos();
            let (dx, dy) = (p.x - x, p.y - y);
            let along = dx * ca + dy * sa;
            let across = -dx * sa + dy * ca - bend * along * along;
            let w = (-(across / width).powi(2)).exp() * smooth(-0.05, 0.1, along) * smooth(0.0, fade.max(0.05), length - along);
            if w < 1.0e-4 {
                continue;
            }
            let v = 0.5 + 0.5 * (std::f32::consts::TAU * across / SCRIM_FOLD + phase).cos();
            h += height * w * (v - 0.5);
        }
        h * taut
    }

    /// The creases' shade at `u` (box units): their height's slope (per box
    /// unit, as fitted) along the way the light comes in, 1 on a smooth scrim.
    fn crease_shade(&self, u: Vec2f) -> f32 {
        if self.creases <= 0.0 {
            return 1.0;
        }
        let p = vec2f(u.x * self.aspect.x, u.y * self.aspect.y);
        let e = 0.004;
        let h0 = self.crease_height(p);
        let hx = (self.crease_height(p + vec2f(e, 0.0)) - h0) / e * self.aspect.x;
        let hy = (self.crease_height(p + vec2f(0.0, e)) - h0) / e * self.aspect.y;
        let l = self.light_dir(u);
        (1.0 + self.creases * (hx * l.x + hy * l.y)).max(0.0)
    }

    /// The mean over the box -1..1, a midpoint sum fine enough for the lamp's
    /// spot (a strip's along x alone).
    fn mean(&self) -> f32 {
        let n = ((4.0 / self.h).ceil() as usize).clamp(64, 400);
        let at = |i: usize, n: usize| 2.0 * (i as f32 + 0.5) / n as f32 - 1.0;
        let sum: f64 = if self.strips == 0 {
            (0..n * n).map(|k| self.at(vec2f(at(k % n, n), at(k / n, n))) as f64).sum::<f64>() / (n * n) as f64
        } else {
            let n = 8 * n;
            (0..n).map(|i| self.at(vec2f(at(i, n), 0.0)) as f64).sum::<f64>() / n as f64
        };
        (sum as f32).max(1.0e-6)
    }
}

use makepad_draw::smoothstep as smooth;

use makepad_draw::mix_f32 as mix;

/// exp(-(x / sigma)²): 1 at 0, e^-1 at sigma.
/// How far past a beauty dish's plate its bright ring falls by 1/e, in radii (RLS001).
const BEAUTY_RING: f32 = 0.09;

fn gauss(x: f32, sigma: f32) -> f32 {
    (-(x * x) / (sigma * sigma)).exp()
}

/// 1 on a line `width` wide at distance `d` from it, fading over `EDGE`.
fn line(d: f32, width: f32) -> f32 {
    1.0 - smooth(0.5 * width, 0.5 * width + EDGE, d.abs())
}

/// The distance in box units from `u` to the nearest of `n` spokes from the
/// middle, the first straight up, along their perpendicular.
fn spoke_gap(u: Vec2f, n: f32, turn: f32) -> f32 {
    let r = u.length();
    if r < 1.0e-6 || n < 1.0 {
        return f32::INFINITY;
    }
    let sector = std::f32::consts::TAU / n;
    let a = u.x.atan2(u.y) + turn;
    let off = (a / sector - (a / sector).round()) * sector;
    r * off.sin().abs()
}

/// The folds of a cloth: ridged value noise, 0..1, 1 on a crest, about 0.43
/// on average (`FOLDS_MEAN`).
fn folds(x: f32, y: f32, seed: u32) -> f32 {
    crate::noise::ridged2(x, y, 3, seed)
}
const FOLDS_MEAN: f32 = 0.43;

/// Folds as a factor round 1: `amount` sets how far the crests rise (and the
/// valleys sink), with the mean kept at 1.
fn creased(x: f32, y: f32, amount: f32, seed: u32) -> f32 {
    1.0 + amount * (folds(x, y, seed) - FOLDS_MEAN) / (1.0 - FOLDS_MEAN)
}

/// A soft cloud over the face, a factor round 1.
fn cloud(x: f32, y: f32, amount: f32, seed: u32) -> f32 {
    1.0 + amount * (2.0 * fbm2(x, y, 3, seed) - 1.0)
}

/// The cells of one hashed point per unit square: the squared distances to
/// the nearest point and the second, and the nearest's own random shade
/// (the open box's glitter, open_box.rs, takes it too).
pub(super) fn cells(x: f32, y: f32, seed: u32) -> (f32, f32, f32) {
    let (ix, iy) = (x.floor(), y.floor());
    let (mut d1, mut d2, mut nearest) = (f32::INFINITY, f32::INFINITY, 0.0f32);
    for j in -1..=1i32 {
        for i in -1..=1i32 {
            let (cx, cy) = (ix as i32 + i, iy as i32 + j);
            let px = ix + i as f32 + hash01(cx, cy, 0, seed);
            let py = iy + j as f32 + hash01(cx, cy, 1, seed);
            let d = (x - px) * (x - px) + (y - py) * (y - py);
            if d < d1 {
                d2 = d1;
                d1 = d;
                nearest = hash01(cx, cy, 2, seed);
            } else if d < d2 {
                d2 = d;
            }
        }
    }
    (d1, d2, nearest)
}

/// A facet's brightness from its random tilt `u` (0..1): log-uniform over
/// a factor e^k, as a mirror's image of the flash goes with the facet's
/// angle. Mean 1 (the mean of e^(k (u - 1/2)) is 2 sinh(k / 2) / k).
pub(super) fn log_uniform(u: f32, k: f32) -> f32 {
    (k * (u - 0.5)).exp() * k / (2.0 * (0.5 * k).sinh())
}

/// The signed distance (in units of the shorter half size, where the box
/// is `aspect`, 1 on its short side) to the front of a cloth face: a
/// rectangle inset `rim` from the box, its corners rounded by `corner` and
/// its sides sagging inward by `sag` at their middles (a diffuser hung
/// between its rods). Negative inside.
fn cloth_sdf(u: Vec2f, aspect: Vec2f, rim: f32, sag: f32, corner: f32) -> f32 {
    let ex = aspect.x - rim - sag * (1.0 - u.y * u.y).max(0.0);
    let ey = aspect.y - rim - sag * (1.0 - u.x * u.x).max(0.0);
    rounded_box(vec2f(u.x * aspect.x, u.y * aspect.y), vec2f(ex, ey), corner)
}

/// The signed distance to a box of half extents `half` with its corners
/// rounded by `corner`, centred on the origin. Negative inside.
pub(crate) fn rounded_box(p: Vec2f, half: Vec2f, corner: f32) -> f32 {
    let q = vec2f(p.x.abs() - half.x + corner, p.y.abs() - half.y + corner);
    vec2f(q.x.max(0.0), q.y.max(0.0)).length() + q.x.max(q.y).min(0.0) - corner
}

/// 1 inside a signed distance's shape, 0 outside, over the soft edge.
fn inside(sdf: f32) -> f32 {
    1.0 - smooth(-EDGE, 0.0, sdf)
}

/// The flash through two layers of cloth, as the plate reads it over its
/// mean: 2.6 at the middle, 1.3 half way, 0.55 at 0.85, falling under the
/// frame to 0.13 at the edge. The hotspot falls over `round` (`cloth_round`:
/// round contours, so it draws no diagonal ridges and the corners fall
/// dimmer than the sides' middles), the frame's dark band over the box
/// distance `depth` (0 at the middle, 1 at the box's edge), over the
/// light the cloth scatters everywhere (`floor`, 0.1 in a 60 x 90 box; a
/// strip's long ends sit higher, RLS021's at 0.3 of the median), the
/// hotspot `spot` of that, the rest spread evenly at `DIFFUSION_EVEN` of
/// the hotspot's peak (`Modifier::spot`). About mean 1 over the box (the
/// softbox's `norm` makes it exact).
fn cloth_falloff(round: f32, depth: f32, floor: f32, spot: f32) -> f32 {
    (floor + 2.5 * (spot * (-2.4 * round * round).exp() + (1.0 - spot) * DIFFUSION_EVEN)) * (1.0 - 0.8 * smooth(0.88, 1.0, depth))
}

/// How far out a point of a cloth face is for its hotspot: the p = 3 norm
/// of `u`, 1 at the sides' middles like the box distance, 1.26 in the
/// corners, its contours rounded squares. The box distance's kink along the
/// diagonals drew an X of ridges across the face; no plate has one.
fn cloth_round(u: Vec2f) -> f32 {
    (u.x.abs().powi(3) + u.y.abs().powi(3)).cbrt()
}

/// One fold of a cloth across a straight line: a lit side and a shaded side
/// (the derivative of a gaussian of `width` across it, peak 1 at a width
/// from the line), so a fold moves light and adds none. `across` is the
/// distance from the line.
fn cloth_fold(across: f32, width: f32) -> f32 {
    let x = across / width;
    1.6487 * x * (-0.5 * x * x).exp()
}

/// The wrinkles of a stretched cloth face as a factor round 1, at `p` in
/// units of the shorter half size (the box's corner at `aspect`): the
/// tension folds the frame's corners pull into it, three from each corner
/// fanning in round its diagonal (soft, 0.4 to 1 of the short half size
/// long, strongest at the corner), and five loose straight creases anywhere
/// (crisp, folded flat once and pulled out again), each a lit and a shaded
/// side (`cloth_fold`). `amount` 1 is the Lightmap cloth faces' reading: their
/// residual over a blur of 6 % of the face has a standard deviation of
/// 0.02 to 0.04 and its 5 and 95 % points at about -0.05 and +0.05.
fn cloth_wrinkles(p: Vec2f, aspect: Vec2f, amount: f32, seed: u32) -> f32 {
    if amount <= 0.0 {
        return 1.0;
    }
    let short = aspect.x.min(aspect.y);
    let mut sum = 0.0f32;
    for i in 0..17i32 {
        let k = |j: i32| hash01(i, j, 3, seed);
        let side = if k(6) < 0.5 { -1.0 } else { 1.0 };
        let (start, (sa, ca), length, width, level, taper) = if i < 12 {
            // A corner's tension folds: from just inside the corner toward the middle, turned up to 25 degrees off it.
            let corner = vec2f(if i & 1 == 0 { aspect.x } else { -aspect.x }, if i & 2 == 0 { aspect.y } else { -aspect.y });
            let inward = (-corner.y).atan2(-corner.x) + 0.87 * (k(0) - 0.5);
            let start = vec2f(corner.x * (1.0 - 0.06 / aspect.x), corner.y * (1.0 - 0.06 / aspect.y));
            (start, inward.sin_cos(), (0.4 + 0.6 * k(1)) * short, 0.02 + 0.035 * k(2), 0.05 * (0.5 + 0.7 * k(3)), true)
        } else {
            // A loose crease: through a hashed point at a hashed angle, centred on it.
            let centre = vec2f((2.0 * k(0) - 1.0) * 0.8 * aspect.x, (2.0 * k(1) - 1.0) * 0.8 * aspect.y);
            let half_length = (0.25 + 0.45 * k(2)) * short;
            let angle = std::f32::consts::PI * k(3);
            let (sa, ca) = angle.sin_cos();
            (vec2f(centre.x - ca * half_length, centre.y - sa * half_length), (sa, ca), 2.0 * half_length, 0.006 + 0.01 * k(4), 0.045 * (0.5 + 0.7 * k(5)), false)
        };
        let (dx, dy) = (p.x - start.x, p.y - start.y);
        let along = (dx * ca + dy * sa) / length;
        if !(-0.1..=1.1).contains(&along) {
            continue;
        }
        let across = dy * ca - dx * sa;
        // A tension fold widens and fades as it runs in; a crease fades at both ends.
        let (w, fade) = if taper {
            (width * (0.6 + 0.8 * along.max(0.0)), smooth(-0.05, 0.08, along) * (1.0 - smooth(0.2, 1.0, along)))
        } else {
            (width, smooth(-0.1, 0.25, along) * (1.0 - smooth(0.75, 1.1, along)))
        };
        sum += side * level * fade * cloth_fold(across, w);
    }
    1.0 + amount * sum
}

/// A closed softbox's front diffuser, drawn by the fabric recipe (the
/// owner's tension map): the frame pulls the cloth at its four corners, and
/// each corner's tension fans in from it (`DIFFUSER_FAN_*`: noise in the
/// fan's own frame, distance along and angle across, domain-warped, shaped
/// between a pinched ridge and a soft fold, weighted 1 / r^2 and fading over
/// its reach); between them broad soft folds where the cloth is slack, a
/// fine crinkle in patches, and the seams across the middle as straight
/// ridges (the tension runs along them). All height, in short half sizes.
const DIFFUSER_FAN_ALONG: f32 = 2.0;
const DIFFUSER_FAN_ACROSS: f32 = 3.0;
const DIFFUSER_FAN_WARP: f32 = 0.3;
const DIFFUSER_FAN_SHARP: f32 = 0.3;
const DIFFUSER_FAN_REACH: f32 = 0.8;
const DIFFUSER_FAN_TIP: f32 = 0.06;
const DIFFUSER_FAN_AMP: f32 = 0.035;
/// An octabox's eight rods pull it more evenly than a softbox's four
/// corners: its fans at this share of a softbox's (RLS017, softly rumpled
/// at its edge).
const OCTA_FANS: f32 = 0.35;
const DIFFUSER_BROAD_FREQ: f32 = 1.2;
const DIFFUSER_BROAD_AMP: f32 = 0.03;
const DIFFUSER_CRINKLE_FREQ: f32 = 50.0;
const DIFFUSER_CRINKLE_AMP: f32 = 0.0018;
const DIFFUSER_SEAM_AMP: f32 = 0.002;
const DIFFUSER_SEAM_WIDTH: f32 = 0.012;
/// An octabox's inner baffle (RLS018, `Modifier::octa_inner`): its cloth
/// inside this octagon distance from the opening's edge, the face's median
/// at this share of its mean, and its tension fans at this share of a
/// softbox's front's.
pub(crate) const OCTA_INNER_EDGE: f32 = 0.26;
const OCTA_MEDIAN: f32 = 1.0 / 1.55;
const OCTA_INNER_FANS: f32 = 0.05;
/// The inner cloth's broad dome under `diffusion`: its level at the middle
/// and its fall, e^(-t / OCTA_INNER_DOME_R), at the core's mean over the
/// inner cloth (about 0.95).
const OCTA_INNER_DOME: f32 = 2.07;
const OCTA_INNER_DOME_R: f32 = 0.6;
/// The inner baffle's walls show the lining's folds at this power: lit
/// through the cloth and from the tube, softer than the open box's.
const OCTA_WALL_CONTRAST: f32 = 0.65;
/// The walls' level per unit of their measured median: what the folded
/// foil's own median (under its mean) leaves them at.
const OCTA_WALLS: f32 = 1.55 * OCTA_MEDIAN;
/// How far behind the diffuser the flash sits, in short half sizes: what
/// lights the folds from behind.
const DIFFUSER_FLASH_DEPTH: f32 = 0.8;

/// The diffuser's height at `p`: the frame pulls it at `pulls` (a softbox's
/// four corners, an octabox's eight rod tips), its slack inside `extent`
/// (the face's half size), the fans `fans` of `DIFFUSER_FAN_AMP`, and `seam`
/// is the distance to the nearest seam.
/// Units: short half sizes, or the octabox's radius.
fn diffuser_height(p: Vec2f, pulls: &[Vec2f], fans: f32, extent: Vec2f, seam: f32, seed: u32) -> f32 {
    let (mut h, mut weights) = (0.0f32, 0.0f32);
    for (k, &corner) in pulls.iter().enumerate() {
        let k = k as u32;
        let d = p - corner;
        let r = d.length() + 1.0e-6;
        let theta = d.y.atan2(d.x) - (-corner.y).atan2(-corner.x);
        let theta = theta - (theta / std::f32::consts::TAU).round() * std::f32::consts::TAU;
        let (along, across) = (r * DIFFUSER_FAN_ALONG, theta * DIFFUSER_FAN_ACROSS);
        let s = seed.wrapping_add(17 * k);
        let seeds = [s.wrapping_add(31), s.wrapping_add(41), s.wrapping_add(51)];
        let profile = fan_folds(along, across, DIFFUSER_FAN_WARP, DIFFUSER_FAN_ACROSS, DIFFUSER_FAN_SHARP, [3.1, 1.7, 8.3, 4.2], 0.0, seeds);
        let reach = (-r / DIFFUSER_FAN_REACH).exp() * (1.0 - (-(r / DIFFUSER_FAN_TIP).powi(2)).exp());
        let w = 1.0 / (r * r);
        h += w * profile * reach;
        weights += w;
    }
    h = fans * DIFFUSER_FAN_AMP * h / weights;
    // Slack between the corners' pulls: the broad soft folds.
    let slack = (1.0 - (p.x / extent.x).powi(4)) * (1.0 - (p.y / extent.y).powi(4));
    let (bx, by) = (p.x * DIFFUSER_BROAD_FREQ, p.y * DIFFUSER_BROAD_FREQ);
    h += DIFFUSER_BROAD_AMP * slack.max(0.0) * broad_folds(bx, by, 0.7, seed.wrapping_add(91), seed.wrapping_add(101));
    // The fine crinkle, in patches.
    let patch = patches(p.x, p.y, 1.5, 0.35, 0.75, 2, seed.wrapping_add(111));
    let (cx, cy) = (p.x * DIFFUSER_CRINKLE_FREQ, p.y * DIFFUSER_CRINKLE_FREQ);
    h += DIFFUSER_CRINKLE_AMP * patch * (fbm2(cx + 5.1, cy + 2.3, 2, seed.wrapping_add(121)) - 0.5);
    // The seams, taut along their length: straight ridges.
    h += DIFFUSER_SEAM_AMP * gauss(seam, DIFFUSER_SEAM_WIDTH);
    h
}

/// A softbox's four corners, the box's corner at `aspect`.
fn box_corners(aspect: Vec2f) -> [Vec2f; 4] {
    [vec2f(aspect.x, aspect.y), vec2f(-aspect.x, aspect.y), vec2f(aspect.x, -aspect.y), vec2f(-aspect.x, -aspect.y)]
}

/// An octabox's eight rod tips on its unit circle, a corner straight up.
fn octa_corners() -> [Vec2f; 8] {
    std::array::from_fn(|k| {
        let (s, c) = (k as f32 * std::f32::consts::TAU / 8.0).sin_cos();
        vec2f(s, c)
    })
}

/// The diffuser's folds as the flash behind it lights them, a factor round 1:
/// the folded cloth's irradiance from the flash (`DIFFUSER_FLASH_DEPTH`
/// behind the middle) over the flat cloth's, through the height's normal.
fn diffuser_shade(p: Vec2f, pulls: &[Vec2f], fans: f32, extent: Vec2f, seam: &dyn Fn(Vec2f) -> f32, seed: u32) -> f32 {
    let e = 0.004;
    let at = |q: Vec2f| diffuser_height(q, pulls, fans, extent, seam(q), seed);
    let h0 = at(p);
    let hx = (at(p + vec2f(e, 0.0)) - h0) / e;
    let hy = (at(p + vec2f(0.0, e)) - h0) / e;
    let n = vec3f(-hx, -hy, 1.0).normalize();
    let l = vec3f(p.x, p.y, DIFFUSER_FLASH_DEPTH).normalize();
    (n.dot(l) / l.z).max(0.0)
}

/// A hotspot over a disc, mean 1 over it: `floor` plus a gaussian of rate
/// `k` (0 is flat), divided by their mean over the unit disc,
/// floor + (1 - floor) (1 - e^-k) / k.
fn hotspot(r: f32, k: f32, floor: f32) -> f32 {
    let mean = floor + (1.0 - floor) * if k > 1.0e-4 { (1.0 - (-k).exp()) / k } else { 1.0 };
    (floor + (1.0 - floor) * (-k * r * r).exp()) / mean
}

/// How far below an umbrella's hub the strobe head's middle sits, in head
/// radii: the shaft runs through the head's umbrella hole above its tube,
/// so the plates (RLS024-026) show the head a fifth of the radius under the
/// hub where the ribs meet.
const HEAD_DROP: f32 = 1.4;

/// The strobe head and its stand in silhouette in front of an umbrella: the
/// reflector dish, a disc of radius `head` `HEAD_DROP` of it under the hub;
/// the head's body behind it, a rounded block showing under the dish; the
/// yoke and the stand's spigot, narrow; and the stand's bar straight down
/// past the edge. 1 where nothing hides the face, and without a head (under
/// 0.01) nothing does.
fn head_and_stand(u: Vec2f, head: f32) -> f32 {
    if head < 0.01 {
        return 1.0;
    }
    let u = vec2f(u.x, u.y + HEAD_DROP * head);
    let dish = 1.0 - smooth(head, head + EDGE, u.length());
    let body = inside(rounded_box(vec2f(u.x, u.y + 0.95 * head), vec2f(0.85 * head, 0.55 * head), 0.35 * head));
    let yoke = inside(rounded_box(vec2f(u.x, u.y + 1.75 * head), vec2f(0.32 * head, 0.45 * head), 0.1 * head));
    let spigot = inside(rounded_box(vec2f(u.x, u.y + 2.5 * head), vec2f(0.6 * STAND_HALF_WIDTH / 0.017 * 0.03, 0.4 * head), 0.02));
    let stand = if u.y < -2.2 * head { 1.0 - smooth(STAND_HALF_WIDTH, STAND_HALF_WIDTH + EDGE, u.x.abs()) } else { 0.0 };
    let clamp = yoke.max(spigot);
    1.0 - 0.985 * dish.max(body).max(clamp).max(stand)
}

/// What shows of `pattern` (a factor about 1 on the face: ribs, spokes and
/// stretchers, folds, the cloth's streaks and weave) where the strobe head's
/// silhouette (`head_and_stand`) is `silhouette`: the head and its stand are
/// in front of all of it, so where they cover the face none of it shows (the
/// silhouette's 1.5 % is the head's own dull light, not a window onto what is
/// behind it), and round them all of it does.
fn behind_head(pattern: f32, silhouette: f32) -> f32 {
    let open = ((silhouette - 0.015) / 0.985).clamp(0.0, 1.0);
    1.0 + (pattern - 1.0) * open
}

/// A reflector's metal spokes, seen from the front: a thin rod from the hub
/// to each corner of the hem, in front of the cloth. The rod is dark (its
/// coat is black) and catches the flash in a thin highlight down one side,
/// where its round section turns toward the hub (RLS025's spokes: a dark line
/// about a hundredth of the radius wide with a lit edge, all the way in).
fn metal_spokes(u: Vec2f, n: f32) -> f32 {
    let r = u.length();
    if r < 1.0e-6 || n < 1.0 {
        return 1.0;
    }
    let sector = std::f32::consts::TAU / n;
    // The ribs run to the hem's corners, half a panel round from the panels'
    // middles (where `scalloped` draws the hem nearest the hub and the seams
    // run).
    let a = u.x.atan2(u.y) + 0.5 * sector;
    let off = (a / sector - (a / sector).round()) * sector;
    // Across the rod, in radii, signed: the lit side is the counter-clockwise one.
    let s = r * off.sin();
    const W: f32 = 0.0075;
    let tips = smooth(0.015, 0.03, r);
    let rod = (-(s / W).powi(2) * 0.5).exp();
    let lit = (-((s - 1.5 * W) / (0.5 * W)).powi(2) * 0.5).exp();
    let ribs = (1.0 - 0.75 * rod * tips) * (1.0 + 1.4 * lit * tips);
    // The stretchers: a thicker strut from the runner (on the shaft, nearer
    // the camera than the hub, so seen below it, toward the head: the struts
    // part from their ribs by a few degrees near the hub) out to each rib,
    // meeting it a little past half way
    // (RLS025's struts end at about 0.55 of the radius; the rib runs on thin
    // to the hem). Dark, a lit edge on the same side as the rib's.
    const SW: f32 = 0.011;
    const REACH: f32 = 0.55;
    // Only the nearest rib's strut can reach a point, and only inside its
    // reach.
    let runner = vec2f(0.0, -0.035);
    if r > REACH + 3.0 * SW {
        return ribs;
    }
    // Near the hub a strut can sit over a neighbouring rib's sector: there
    // the two neighbours are tried too.
    let k = (a / sector).round();
    let span = if r < 0.25 { 1 } else { 0 };
    let mut struts = 1.0;
    for j in -span..=span {
        let ang = (k + j as f32 - 0.5) * sector;
        let tip = vec2f(REACH * ang.sin(), REACH * ang.cos());
        let dir = tip - runner;
        let len = dir.length();
        let dn = vec2f(dir.x / len, dir.y / len);
        let q = u - runner;
        // Signed distance across the strut, counter-clockwise positive.
        let side = q.x * dn.y - q.y * dn.x;
        if side.abs() > 4.0 * SW {
            continue;
        }
        let proj = q.x * dn.x + q.y * dn.y;
        let t = proj.clamp(0.0, len);
        let along_end = smooth(len - 0.02, len, t);
        let w = SW * (1.0 - 0.35 * along_end);
        let cap = (-((t - proj) / w).powi(2) * 0.5).exp();
        let body = (-(side / w).powi(2) * 0.5).exp() * cap;
        let edge = (-((side + 1.5 * w) / (0.45 * w)).powi(2) * 0.5).exp() * cap * (1.0 - along_end);
        let start = smooth(0.02, 0.05, t);
        struts *= (1.0 - 0.8 * body * start) * (1.0 + 1.1 * edge * start);
    }
    ribs * struts
}

/// The ribs of an umbrella as thin lines from the hub outward, the way the
/// plates draw them (half a percent of the diameter, a texel or two at 4K):
/// dark at a positive `depth` (a rib's shadow), light at a negative one (a
/// lit rib); and the hub where they meet, a small dark ferrule.
fn rib_lines(u: Vec2f, n: f32, depth: f32) -> f32 {
    let r = u.length();
    let rib = 1.0 - smooth(0.004, 0.011, spoke_gap(u, n, 0.0));
    let hub = 1.0 - 0.85 * (1.0 - smooth(0.014, 0.022, r));
    (1.0 - depth * rib * smooth(0.02, 0.05, r)) * hub
}

/// A white umbrella's ribs: a thin metal rod from the hub to each corner of
/// the hem (half a panel round from the panels' middles, where `scalloped`
/// draws the hem nearest the hub), its round section lit down the middle
/// (`light` over the cloth) and dark at both edges, with a joint a little
/// past half way, a short sleeve a touch wider and dark at its ends
/// (RLS024's ribs: a hundredth of the radius wide, the joint at 0.55), the
/// dark edges, the sleeve and the hub where they meet (a small dark ferrule)
/// at `metal`; 0 draws nothing, as a parabolic's ribs behind its front
/// diffuser.
fn umbrella_ribs(u: Vec2f, n: f32, light: f32, metal: f32) -> f32 {
    let r = u.length();
    let hub = 1.0 - 0.85 * metal * (1.0 - smooth(0.014, 0.022, r));
    if r < 1.0e-6 || n < 1.0 {
        return hub;
    }
    let sector = std::f32::consts::TAU / n;
    let a = u.x.atan2(u.y) + 0.5 * sector;
    let off = (a / sector - (a / sector).round()) * sector;
    let s = r * off.sin();
    const W: f32 = 0.005;
    const JOINT: f32 = 0.55;
    let sleeve = gauss(r - JOINT, 0.02);
    let w = W * (1.0 + 0.4 * sleeve);
    if s.abs() > 4.0 * w {
        return hub;
    }
    let core = gauss(s, 0.6 * w);
    let edges = gauss(s.abs() - w, 0.45 * w);
    let sleeve_ends = gauss((r - JOINT).abs() - 0.02, 0.005) * gauss(s, 1.3 * w);
    let tips = smooth(0.02, 0.05, r);
    (1.0 + light * core * tips) * (1.0 - metal * (0.5 * edges + 0.45 * sleeve_ends) * tips) * hub
}

/// The height of a white umbrella's cloth from its tension map (the owner's
/// fabric recipe): each rib tip at a corner of the hem pulls the cloth
/// toward it, a fan of folds running in from the tip; each tip's pull
/// weighted by 1 / r^2 and fading with its reach. The folds are sampled in
/// the fan's own frame, polar round the tip (the distance along the pull,
/// stretched; the angle across it, squeezed, so the folds spread as they run
/// in), domain-warped by low noise so they wander and branch, and shaped by
/// the crease profile (a pinched ridge mixed with a soft fold). Box units.
fn umbrella_height(u: Vec2f, n: f32, edge: f32, seed: u32) -> f32 {
    // The pull's share of the folds: strongest by the tips, none at the hub.
    let strength = smooth(0.35, 0.85, u.length());
    if strength <= 0.0 {
        return 0.0;
    }
    let sector = std::f32::consts::TAU / n;
    let a = u.x.atan2(u.y) + 0.5 * sector;
    // Only the tips at the panel's two corners reach a point: each tip's
    // reach ends short of the next rib but one.
    let p = (a / sector).floor();
    let end = (0.85 * edge * sector.sin()).min(0.55);
    let (mut h, mut wsum) = (0.0f32, 1.0f32);
    for dk in 0..2 {
        let k = p + dk as f32;
        let ki = (k as i32).rem_euclid(n.max(1.0) as i32);
        let ang = (k - 0.5) * sector;
        let tip = vec2f(edge * ang.sin(), edge * ang.cos());
        let q = u - tip;
        let rho = q.length();
        if rho > end {
            continue;
        }
        // The angle off the line back to the hub, -pi..pi (the cloth lies
        // within about 70 degrees of it).
        let inward = ang + std::f32::consts::PI;
        let mut th = q.x.atan2(q.y) - inward;
        th -= std::f32::consts::TAU * (th / std::f32::consts::TAU).round();
        let reach = smooth(0.02, 0.06, rho) * (1.0 - smooth(0.45 * end, end, rho));
        let w = reach / (rho * rho + 0.02);
        let sd = seed.wrapping_add(ki as u32 * 7919);
        let warp = fbm2(rho * 3.0, th * 1.5, 2, sd ^ 0x51) - 0.5;
        let along = rho * 1.8 + 1.2 * warp;
        let across = th * 3.5 + 1.6 * warp;
        let v = ((fbm2(across, along, 2, sd ^ 0x77) - 0.5) * 1.8 + 0.5).clamp(0.0, 1.0);
        let profile = 0.7 * (1.0 - (2.0 * v - 1.0).abs()) + 0.3 * v;
        h += w * profile;
        wsum += w;
    }
    strength * h / wsum
}

/// A white umbrella's folds lit by the flash: the cloth's height
/// (`umbrella_height`) turned into a normal by its slope and lit from the
/// flash's side (`flash`, the head's middle under the hub), so a fold shows
/// a lit flank toward the flash and a shaded one away from it. A factor
/// round 1; `amount` 1 is RLS024's reading.
fn umbrella_folds(u: Vec2f, n: f32, edge: f32, flash: Vec2f, amount: f32, seed: u32) -> f32 {
    if amount <= 0.0 || n < 3.0 {
        return 1.0;
    }
    const D: f32 = 0.004;
    let h0 = umbrella_height(u, n, edge, seed);
    let hx = umbrella_height(vec2f(u.x + D, u.y), n, edge, seed);
    let hy = umbrella_height(vec2f(u.x, u.y + D), n, edge, seed);
    let (gx, gy) = ((hx - h0) / D, (hy - h0) / D);
    let to = flash - u;
    let len = to.length().max(1.0e-3);
    let slope = (gx * to.x + gy * to.y) / len;
    (1.0 + amount * 0.007 * slope).max(0.2)
}

/// A parabolic's front diffuser, stretched over its rib tips (RLS028), as
/// the cloth's height from its tension map (the owner's fabric recipe): the
/// puffs, where between each pair of tips the cloth bulges out a little by
/// the hem, highest half way between them and pinched to nothing at the
/// tips (`in_r` is how far in from the hem); and the billows, broad soft
/// folds across the face, the slack the frame leaves, long along one slant
/// (a low warped noise, about one and a half across).
fn front_height(u: Vec2f, n: f32, in_r: f32, seed: u32) -> (f32, f32) {
    let sector = std::f32::consts::TAU / n;
    let a = u.x.atan2(u.y) + 0.5 * sector;
    let f = a / sector - (a / sector).floor();
    let between = (std::f32::consts::PI * f).sin().powi(2);
    let puff = between * gauss(in_r - 0.09, 0.07);
    // The billows run long along a slant 30 degrees up from the horizontal.
    let (sa, ca) = (0.5f32, 0.866f32);
    let (p, q) = (u.x * ca + u.y * sa, -u.x * sa + u.y * ca);
    let w = fbm2(p * 0.8 + 3.1, q * 0.8 + 1.7, 2, seed ^ 0x2f) - 0.5;
    let billow = fbm2(p * 0.45 + 0.6 * w, q * 0.9 - 0.6 * w, 2, seed ^ 0x3d) - 0.5;
    (puff, billow)
}

/// A parabolic's front diffuser lit by the flash behind it: the cloth's
/// height (`front_height`) turned into a normal by its slope. The puffs are
/// lit from the bowl's middle, their inner flanks bright (RLS028's bright
/// lobes inside the rim, dark notches at the tips); the billows from the
/// flash's slant, a bright band with darker zones either side. And the
/// frame's black binding round the hem, 0.035 of the radius wide, with a
/// thin bright lip where the cloth turns over it. `polygon` is the hem's
/// distance from the middle at this angle. A factor round 1; `amount` 0
/// leaves the face as it is, and `calm` scales the puffs and billows (a
/// second diffuser inside hides them, `Modifier::cover`).
fn front_cloth(u: Vec2f, n: f32, polygon: f32, amount: f32, calm: f32, seed: u32) -> f32 {
    if amount <= 0.0 || n < 3.0 {
        return 1.0;
    }
    const D: f32 = 0.004;
    let r = u.length();
    let in_r = polygon - r;
    let (p0, b0) = front_height(u, n, in_r, seed);
    let ux = vec2f(u.x + D, u.y);
    let uy = vec2f(u.x, u.y + D);
    let (px, bx) = front_height(ux, n, polygon - ux.length(), seed);
    let (py, by) = front_height(uy, n, polygon - uy.length(), seed);
    let len = r.max(1.0e-3);
    let puff_slope = ((px - p0) * u.x + (py - p0) * u.y) / (D * len);
    let billow_slope = ((bx - b0) * -0.6 + (by - b0) * 0.8) / D;
    let lit = (1.0 + 0.03 * puff_slope * calm + 1.1 * billow_slope * calm).max(0.2);
    let binding = 0.08 + 0.92 * smooth(0.03, 0.04, in_r);
    let lip = 1.0 + 0.25 * gauss(in_r - 0.045, 0.006);
    1.0 + amount * (lit * binding * lip - 1.0)
}

/// A diffuser over an umbrella's face (`Modifier::cover`) lit from behind:
/// its height (`cover_height`) turned into a normal by its slope and lit
/// from the middle of the bowl behind it, as a front diffuser's are
/// (`diffuser_shade`). A factor round 1, `amount` 0 flat.
fn cover_folds(u: Vec2f, n: f32, edge: f32, amount: f32, gathered: f32, seed: u32) -> f32 {
    if amount <= 0.0 || n < 3.0 {
        return 1.0;
    }
    const D: f32 = 0.004;
    let h0 = cover_height(u, n, edge, gathered, seed);
    let hx = (cover_height(vec2f(u.x + D, u.y), n, edge, gathered, seed) - h0) / D * amount;
    let hy = (cover_height(vec2f(u.x, u.y + D), n, edge, gathered, seed) - h0) / D * amount;
    let normal = vec3f(-hx, -hy, 1.0).normalize();
    let l = vec3f(u.x, u.y, COVER_DEPTH).normalize();
    (normal.dot(l) / l.z).max(0.0)
}

/// The height of a diffuser over an umbrella's face, from its tension map:
/// the rib tips' fans (`umbrella_height`, `COVER_FOLDS` high), and where the
/// hem gathers the cloth (`gathered`: a parabolic's second diffuser; a
/// white umbrella's cover is taut), the band inside it (`COVER_GATHER_IN`),
/// broad soft folds and puffs (noise in polar coordinates, round the ring
/// about `COVER_GATHER_ACROSS` a panel, `COVER_GATHER_ALONG` a radius out,
/// domain-warped so they wander and branch, a soft fold shaped by a little
/// pinch), `COVER_GATHER` high. Box units.
fn cover_height(u: Vec2f, n: f32, edge: f32, gathered: f32, seed: u32) -> f32 {
    let r = u.length();
    let fans = COVER_FOLDS * umbrella_height(u, n, edge, seed);
    let band = gathered * smooth(edge - COVER_GATHER_IN, edge - 0.5 * COVER_GATHER_IN, r) * (1.0 - smooth(edge - 0.04, edge, r));
    if band <= 0.0 {
        return fans;
    }
    // Round the ring on a circle of the noise (so it closes on itself), out
    // along the third axis.
    let (ca, sa) = (u.x / r.max(1.0e-6), u.y / r.max(1.0e-6));
    let k = n * COVER_GATHER_ACROSS / std::f32::consts::TAU;
    let along = r * COVER_GATHER_ALONG;
    let warp = fbm3(ca * 0.4 * k + 2.3, sa * 0.4 * k + 5.1, along * 0.5 + 7.1, 2, seed ^ 0x91) - 0.5;
    let v = fbm3(ca * k + 0.6 * warp, sa * k - 0.6 * warp, along + 1.2 * warp, 2, seed ^ 0xa3);
    let profile = 0.15 * (1.0 - (2.0 * v - 1.0).abs()) + 0.85 * v;
    fans + COVER_GATHER * band * (profile - 0.5)
}

/// The mean of `f` over the unit disc (`round`) or the box -1..1, a 48 x 48
/// midpoint sum: what a face's pattern is divided by to sit at mean 1.
fn grid_mean(round: bool, f: impl Fn(Vec2f) -> f32) -> f32 {
    let n = 48;
    let (mut sum, mut count) = (0.0f64, 0u32);
    for j in 0..n {
        for i in 0..n {
            let u = vec2f(2.0 * (i as f32 + 0.5) / n as f32 - 1.0, 2.0 * (j as f32 + 0.5) / n as f32 - 1.0);
            if round && u.length() > 1.0 {
                continue;
            }
            sum += f(u) as f64;
            count += 1;
        }
    }
    ((sum / count.max(1) as f64) as f32).max(1.0e-3)
}

impl Modifier {
    /// The modifier of `p` (as clamp() holds it) on a face whose tangent half
    /// extents are `half`. None for "none" and for a window (a gobo, not a
    /// pattern on the face).
    pub fn new(p: &ModifierParams, half: Vec2f) -> Option<Modifier> {
        Self::new_shaped(p, half, false)
    }

    /// `new` on a light of the round shape (`round`, a disc): a reflector is
    /// then a round card, its outline and binding on the circle. Every other
    /// kind draws the same on either.
    pub fn new_shaped(p: &ModifierParams, half: Vec2f, round: bool) -> Option<Modifier> {
        let p = p.clamped();
        let kind = p.kind();
        if matches!(kind, ModifierKind::None | ModifierKind::Window) {
            return None;
        }
        let half = vec2f(half.x.max(1.0e-6), half.y.max(1.0e-6));
        let seed = hash_u32(half.x.to_bits() ^ hash_u32(half.y.to_bits()).rotate_left(7) ^ (kind as u32).wrapping_mul(0x9e37_79b9));
        let short = half.x.min(half.y);
        // The silver lining: an open box's walls, and the inner baffle's band
        // (the same walls seen round the inner cloth, their folds only).
        let open = match kind {
            ModifierKind::Softbox | ModifierKind::Strip if p.baffle > 0.05 => Some(OpenBox::new(false, vec2f(half.x / short, half.y / short), TUBE_SHARE_BOX, seed)),
            ModifierKind::Octabox if p.baffle > 0.0 => Some(OpenBox::new(true, vec2f(1.0, 1.0), TUBE_SHARE_OCTA, seed)),
            _ => None,
        };
        let deep = match kind {
            ModifierKind::SilverUmbrella if p.baffle >= 0.75 => Some(DeepSilver::new(if p.ribs >= 3 { p.ribs } else { 16 }, seed)),
            _ => None,
        };
        let shallow = match kind {
            ModifierKind::SilverUmbrella if p.baffle < 0.75 => Some(DeepSilver::shallow(if p.ribs >= 3 { p.ribs } else { 16 }, seed)),
            _ => None,
        };
        let reflector = match kind {
            ModifierKind::Reflector => Some(Reflector::new(half, &p, round, seed)),
            _ => None,
        };
        let scrim = match kind {
            ModifierKind::Gradient => Some(Scrim::new(&p, half)),
            _ => None,
        };
        let mut m = Modifier { kind, half, rim: p.rim, baffle: p.baffle, ribs: p.ribs as f32, plate: p.plate, diffusion: p.diffusion.unwrap_or(0.0), seed, norm: 1.0, open, deep, shallow, reflector, scrim, tint_mean: None };
        if kind == ModifierKind::Fluorescent {
            (m.norm, m.tint_mean) = m.fluorescent_mean_tint();
        } else {
            m.norm = m.face_norm();
        }
        Some(m)
    }

    /// What the faces' patterns are divided by to sit at mean 1: a
    /// softbox's or strip's diffused face over the box, an umbrella's hot
    /// zone (round the flash, under the hub) over the disc; a bare head's or a
    /// beauty dish's face over the disc. 1 for the rest.
    fn face_norm(&self) -> f32 {
        match self.kind {
            ModifierKind::Softbox | ModifierKind::Strip => grid_mean(false, |u| self.diffused(u)),
            ModifierKind::Umbrella => grid_mean(true, |u| self.umbrella_hot(u)),
            ModifierKind::SilverUmbrella => grid_mean(true, |u| Self::silver_profile_at(u.length(), self.silver_depth(), 0.0)),
            ModifierKind::Bulb => self.disc_mean(false, Self::bulb_raw),
            ModifierKind::BeautyDish => self.disc_mean(false, Self::beauty_raw),
            // The lens takes what the barrel leaves of the mean.
            ModifierKind::Fresnel => self.disc_mean(false, Self::fresnel_lens) / (1.0 - self.disc_mean(false, Self::fresnel_barrel)).max(0.5),
            ModifierKind::LedPanel if self.ribs < 1.0 => self.disc_mean(false, |m, u, r| m.nude_led_raw(u, r, false)),
            ModifierKind::LedPanel => Self::led_diffuser_mean(self.half, self.rim, self.baffle, self.seed),
            ModifierKind::Gradient => self.scrim.as_ref().map_or(1.0, Scrim::mean),
            ModifierKind::RingLight => self.ring_mean(),
            ModifierKind::Fluorescent => self.fluorescent_mean_tint().0,
            ModifierKind::Candle => grid_mean(true, super::light_model::candle::flame_face),
            _ => 1.0,
        }
    }

    /// The face's emission at `q` as a share of the light's: about 1 over the
    /// face, 0.04 in the housing, far above at a hotspot. 1 where `q` is not
    /// a point (a NaN direction leaves the light as it is).
    pub fn factor(&self, q: Vec2f) -> f32 {
        self.factor_with(q, true)
    }

    /// `factor` without what a light's 3D model draws as geometry in front
    /// of the face (light_model.rs): an umbrella's strobe head and its stand,
    /// a bare beauty dish's plate and its clips (`beauty_with`). Every other
    /// face is `factor`'s.
    pub fn factor_bare(&self, q: Vec2f) -> f32 {
        self.factor_with(q, false)
    }

    /// The colour the face takes at `q` over its level (luminance 1, `factor` carries the level), None where it
    /// keeps the light's own: a dirty prismatic cover's stains pass red more than blue (`fluo_dirt_depth`).
    pub fn tint(&self, q: Vec2f) -> Option<Vec3f> {
        self.factor_tint_with(q, true, 0.0).1
    }

    /// `factor` and `tint` together, worked out once, the face's fine detail band-limited to a map's texel
    /// `texel` (radians; 0 for its finest).
    pub fn factor_tint(&self, q: Vec2f, texel: f32) -> (f32, Option<Vec3f>) {
        self.factor_tint_with(q, true, texel)
    }

    /// `factor_bare` and `tint` together (`factor_tint`).
    pub fn factor_bare_tint(&self, q: Vec2f, texel: f32) -> (f32, Option<Vec3f>) {
        self.factor_tint_with(q, false, texel)
    }

    /// The colour the face's light takes on average over the face, weighted by its level (luminance 1), None
    /// where it keeps the light's own: what the engine's key carries of `tint`.
    pub fn tint_mean(&self) -> Option<Vec3f> {
        self.tint_mean
    }

    /// `factor`; `head` false leaves out the strobe head and its stand.
    fn factor_with(&self, q: Vec2f, head: bool) -> f32 {
        self.factor_tint_with(q, head, 0.0).0
    }

    /// `factor_with` and the face's colour there (`tint`), at a map's texel `texel` (radians).
    fn factor_tint_with(&self, q: Vec2f, head: bool, texel: f32) -> (f32, Option<Vec3f>) {
        if !q.x.is_finite() || !q.y.is_finite() {
            return (1.0, None);
        }
        let mut tint = None;
        let u = vec2f(q.x / self.half.x, q.y / self.half.y);
        // How far in from the box's edge, 0 at it and 1 at the middle: a box's
        // (softbox, strip, panel) or a disc's (the round ones). The rim is that
        // share of the smaller half size.
        let boxed = 1.0 - u.x.abs().max(u.y.abs());
        let r = u.length();
        let round = 1.0 - r;
        let in_box = |inset: f32| smooth(inset, inset + EDGE, boxed);
        let housing = |inside: f32| HOUSING + (1.0 - HOUSING) * inside;
        // The face's aspect: a hotspot is round in the real face, so on a strip
        // it runs along the length and is flat across (`d`, 1 at the far ends).
        let longest = self.half.x.max(self.half.y);
        let (ax, ay) = (self.half.x / longest, self.half.y / longest);
        let d = ((u.x * ax).powi(2) + (u.y * ay).powi(2)).sqrt();
        // Folds and clouds are laid out in real units too: `fx`, `fy` run
        // 0..about 2 over the longer side.
        let (fx, fy) = ((u.x + 1.0) * ax, (u.y + 1.0) * ay);
        let core = self.tube(d);
        let f = core
            + match self.kind {
                ModifierKind::Softbox | ModifierKind::Strip => housing(inside(cloth_sdf(u, self.aspect(), self.rim, 0.015, 0.06))) * self.softbox(u),
                ModifierKind::Octabox => self.octabox(u, r, fx, fy),
                ModifierKind::Umbrella => self.umbrella(u, r, head),
                ModifierKind::SilverUmbrella => match &self.deep {
                    Some(deep) if head => deep.factor(u),
                    Some(deep) => deep.factor_bare(u),
                    None => self.silver_umbrella(u, r, head),
                },
                ModifierKind::BeautyDish if !head => self.beauty_with(u, r, false) / self.norm,
                ModifierKind::BeautyDish => self.beauty_dish(u, r),
                ModifierKind::RingLight => housing(Self::ring_edge(round, self.rim)) * self.ring_light(u, r),
                ModifierKind::Fresnel => self.fresnel(u, r),
                ModifierKind::Bulb if !head => self.bulb_with(u, r, false) / self.norm,
                ModifierKind::Bulb => self.bulb(u, r),
                ModifierKind::LedPanel if self.ribs < 1.0 => self.nude_led_raw(u, r, true) / self.norm,
                ModifierKind::LedPanel => housing(in_box(self.rim)) * self.led_panel(u, d),
                ModifierKind::Gradient => housing(in_box(self.rim)) * self.gradient(u),
                ModifierKind::StreetLamp => self.street_lamp(u, r),
                ModifierKind::Reflector => self.reflector.as_ref().map_or(1.0, |p| p.factor(u)),
                ModifierKind::Fluorescent => {
                    // (the texel in short half sizes of the face where `q` lies on the frame's plane)
                    let short = self.half.x.min(self.half.y);
                    let (f, t) = self.fluorescent(u, texel * (1.0 + q.x * q.x + q.y * q.y) / short);
                    tint = t;
                    f
                }
                ModifierKind::Candle => super::light_model::candle::flame_face(u) / self.norm,
                ModifierKind::None | ModifierKind::Window => 1.0,
            };
        if f.is_finite() {
            (f.max(0.0), tint)
        } else {
            (1.0, None)
        }
    }

    /// The bare tube at the middle of an open box, which the plates put at
    /// 0.77 to 0.87 of the face's light: a crisp disc (round in angle: `d`
    /// is the real-unit radius) at the level that gives it `core_mass`, so
    /// about 250 x the face's mean at the middle of a 60 x 90 box. 0 without
    /// a core. No halo round it (owner, 2026-11-07: the dark zone round the
    /// ring "looks more like fog"): a halo of a fifth of a percent of the
    /// tube outshone the walls' own light out to a sixth of the half size,
    /// and the plates' glow there is their camera's flare, never baked.
    fn tube(&self, d: f32) -> f32 {
        let mass = self.core_mass();
        if mass <= 0.0 {
            return 0.0;
        }
        let radius = self.core_radius();
        let dq = d * self.half.x.max(self.half.y);
        let level = mass / (TUBE_DISC_AREA * std::f32::consts::PI * radius * radius);
        level * (1.0 - smooth(0.85 * radius, 1.15 * radius, dq))
    }

    /// The radius of the bare tube's crisp disc (`tube`), in the face's
    /// units: `TUBE_RADIUS_BOX` or `TUBE_RADIUS_OCTA` of the shorter half
    /// size, never under `TUBE_MIN_TAN`. Only meaningful where `core_mass`
    /// is above 0.
    pub fn core_radius(&self) -> f32 {
        let share = if self.kind == ModifierKind::Octabox { TUBE_RADIUS_OCTA } else { TUBE_RADIUS_BOX };
        (share * self.half.x.min(self.half.y)).max(TUBE_MIN_TAN)
    }

    /// The face's hot core at `q` (the tube of an open box), the part of
    /// `factor` too small for a key's strata to hit reliably: `factor` is the
    /// face without it plus this, and `core_mass` its integral.
    pub fn core_at(&self, q: Vec2f) -> f32 {
        if !q.x.is_finite() || !q.y.is_finite() {
            return 0.0;
        }
        self.tube(q.length() / self.half.x.max(self.half.y))
    }

    /// The integral of `core_at` over the face, in tangent units squared:
    /// an open box's bare tube (`baffle` past 0.75), `TUBE_SHARE_BOX` or
    /// `TUBE_SHARE_OCTA` of a face whose mean is 1 times the face's area
    /// (the box's, or the octagon's share of it). 0 for every other face.
    pub fn core_mass(&self) -> f32 {
        let open = smooth(0.75, 1.0, self.baffle);
        let area = 4.0 * self.half.x * self.half.y;
        match self.kind {
            ModifierKind::Softbox | ModifierKind::Strip => TUBE_SHARE_BOX * open * area,
            ModifierKind::Octabox => TUBE_SHARE_OCTA * open * OCTAGON_SHARE * area,
            _ => 0.0,
        }
    }

    /// A softbox or a strip: the flash through two layers of cloth
    /// (`cloth_falloff`: 2.6 x the face's mean at the middle, dark at the
    /// edges under the frame), faint seams and folds. With the inner baffle
    /// alone (`baffle` toward 0.7) the front diffuser is off: a smaller
    /// rounded cloth sits back in the box, the flash's core 14 x on it, its
    /// folds stronger, framed by the box's silver walls, a lit band of
    /// folded foil brightest at 0.8 of the way out and darker under the
    /// frame, a shadow line where the cloth meets them. Open (toward 1) the
    /// bare tube (`tube`, 0.77 of the light) sits at the back of the box's
    /// silver lining, folded and lit (open_box.rs).
    fn softbox(&self, u: Vec2f) -> f32 {
        let open = smooth(0.75, 1.0, self.baffle);
        let diffused = if open < 1.0 { self.diffused(u) / self.norm } else { 0.0 };
        if open <= 0.0 {
            return diffused;
        }
        // The open box: the silver lining as the tube lights it.
        let walls = self.open.as_ref().map_or(0.0, |lining| lining.walls(u));
        mix(diffused, walls, open)
    }

    /// A softbox's or strip's face behind its diffusers, before `norm`: the
    /// flash through both cloths (`cloth_falloff`), the cloth's soft cloud,
    /// its tension folds and creases (`cloth_wrinkles`) and its seams; with the
    /// inner baffle alone (`baffle` toward 0.7) the smaller cloth back in the
    /// box, the flash's core on it a four-pointed star (RLS011: brightest
    /// along the box's axes), framed by the walls' band of dim grey foil,
    /// darker under the frame and in the corners' seams, with the cloth's
    /// shadow line on it.
    fn diffused(&self, u: Vec2f) -> f32 {
        let open = smooth(0.75, 1.0, self.baffle);
        let inner = smooth(0.0, 0.7, self.baffle) * (1.0 - open);
        let r = u.length();
        let depth = u.x.abs().max(u.y.abs());
        let longest = self.half.x.max(self.half.y);
        let (fx, fy) = ((u.x + 1.0) * self.half.x / longest, (u.y + 1.0) * self.half.y / longest);
        let aspect = self.aspect();
        // The hotspot is rounder in the real face than the box: half way from
        // the box's units to real ones (RLS010's hot zone fills the width of
        // a tall box and fades up and down it; RLS021's is a band across the
        // strip's middle).
        let geo = (aspect.x * aspect.y).sqrt();
        let (sx, sy) = ((aspect.x / geo).powf(0.45), (aspect.y / geo).powf(0.45));
        let floor = 0.085 * (aspect.x.max(aspect.y) / aspect.x.min(aspect.y)).powf(0.9);
        let falloff = cloth_falloff(cloth_round(vec2f(u.x * sx, u.y * sy)), depth, floor, self.spot());
        let folds = cloth_wrinkles(vec2f(u.x * aspect.x, u.y * aspect.y), aspect, 1.6 + 0.4 * inner, self.seed ^ 0x3c1);
        let cloth = creased(fx * 3.0, fy * 3.0, 0.03 + 0.05 * inner, self.seed) * cloud(fx * 1.5, fy * 1.5, 0.04 + 0.06 * inner, self.seed ^ 0x11) * cloud(fx * 6.0, fy * 6.0, 0.06, self.seed ^ 0x2d) * folds;
        let seam = 1.0 - 0.05 * line(u.x, 0.01).max(line(u.y, 0.01));
        // The front diffuser's folds and seams from the tension map
        // (`diffuser_shade`); the inner baffle's cloth keeps its own.
        let front = falloff * diffuser_shade(vec2f(u.x * aspect.x, u.y * aspect.y), &box_corners(aspect), 1.0, aspect, &|q| q.x.abs().min(q.y.abs()), self.seed ^ 0x3c1);
        let closed = mix(front, falloff * cloth * seam, smooth(0.0, 0.15, inner));
        if inner <= 0.0 {
            return closed;
        }
        // How far in from the box's edge a point is (in short half sizes),
        // across the box and up it; the walls' band between the edge and
        // the inner cloth, the top and bottom walls twice as deep as the
        // sides (RLS011: the cloth inset 0.19 at the sides, 0.38 at the top
        // and bottom, as the box is seen from in front of its middle).
        let (in_x, in_y) = ((1.0 - u.x.abs()) * aspect.x, (1.0 - u.y.abs()) * aspect.y);
        // A strip's inner cloth sits deeper (`STRIP_BAFFLE_*`, RLS022).
        let strip = self.kind == ModifierKind::Strip;
        let (deep_x, deep_y) = if strip { (STRIP_BAFFLE_SIDE, STRIP_BAFFLE_END) } else { (BAFFLE_SIDE, 2.0 * BAFFLE_SIDE) };
        let on_sides = in_x * deep_y < in_y * deep_x;
        let side = if on_sides { i32::from(u.x < 0.0) } else { 2 + i32::from(u.y < 0.0) };
        let shade = 0.9 + 0.2 * hash01(side, 0, 0, self.seed ^ 0x5e);
        // The corner's seam: the line from the box's corner to the cloth's.
        let diagonal = (in_x * deep_y - in_y * deep_x).abs() / deep_x.hypot(deep_y);
        // The inner cloth, rounded and sagging; the flash's core on it, its
        // broad part a star of four points along the axes (cos 2a to the
        // fourth: 1 on the axes, 0 on the diagonals) reaching 1.8 x as far
        // as between them.
        let sag = |w: f32| 0.015 * (1.0 - w * w).max(0.0);
        let inner_sdf = rounded_box(vec2f(u.x * aspect.x, u.y * aspect.y), vec2f(aspect.x - deep_x - sag(u.y), aspect.y - deep_y - sag(u.x)), 0.08);
        let star = (u.x * u.x - u.y * u.y) / (r * r).max(1.0e-6);
        let star_r = r / (1.0 + 0.8 * star * star * star * star);
        let p = vec2f(u.x * aspect.x, u.y * aspect.y);
        let inner_face = if strip {
            let half = vec2f(aspect.x - deep_x, aspect.y - deep_y);
            Self::strip_core(p) * Self::strip_cloth(p, half, self.seed)
        } else {
            (0.55 * falloff + 14.0 * gauss(r, 0.09) + 3.0 * gauss(star_r, 0.28)) * cloth * seam
        };
        // The walls' band: the silver lining's walls seen round the cloth,
        // folded by the fabric's mechanics (`OpenBox::folds`: the tension
        // fans from the corners, the creases lit by the light), at the
        // levels measured on RLS011 over its mean: the sides 0.43, the top
        // and bottom 0.25; dark under the frame to 0.07 in at the sides and
        // 0.11 at the top and bottom; a thin lit edge where the cloth
        // begins, a dark seam down each corner. Levels here are before
        // `norm`.
        let inset = if on_sides { in_x } else { in_y };
        let frame = if on_sides { 0.07 } else { 0.11 };
        let under_frame = smooth(frame - 0.02, frame + 0.015, inset);
        let walls_level = match (strip, on_sides) {
            // A strip's sides lit by the flash beside them as its cloth is, its
            // ends dark (RLS022).
            (true, true) => STRIP_SIDE_WALLS * Self::strip_core(vec2f(0.0, p.y)),
            (true, false) => STRIP_END_WALLS,
            (false, true) => 0.55,
            (false, false) => 0.32,
        };
        // The sides are calmer than the top and bottom (RLS011: their folds
        // at half the contrast, the darkest tenth 0.3 of their median): the
        // tension folds from the rod tips run along them.
        let folds = self.open.as_ref().map_or(1.0, |lining| lining.folds(u));
        let texture = if on_sides { (1.0 + 0.5 * (folds - 1.0)).max(0.3) } else { folds };
        let edge = 1.0 + 1.5 * line(inner_sdf - 0.012, 0.012);
        let corners = (1.0 - 0.6 * line(diagonal, 0.015)) * (1.0 - 0.3 * gauss(diagonal, 0.06));
        let walls = walls_level * shade * under_frame * edge * corners * texture;
        mix(closed, mix(walls, inner_face, inside(inner_sdf)), inner)
    }

    /// A strip's inner baffle as RLS022 reads over its median along its
    /// middle, at `p` in short half sizes: the flash's core, round in the
    /// real face but a little wider across, 32 x at the middle, a third of
    /// that a third of a short half size out, falling as e^(-d / 0.52) to
    /// 0.75 x by the cloth's ends.
    fn strip_core(p: Vec2f) -> f32 {
        let d = vec2f(p.x / 1.6, p.y).length();
        0.7 + 21.0 * (-d / 0.52).exp() + 10.0 * (-d / 0.15).exp()
    }

    /// A strip's inner cloth (inside `half`, in short half sizes) from its
    /// tension map (`diffuser_shade`: its four corners pull it), lit by the
    /// flash behind it, its relief at `STRIP_RELIEF` of the front's (the
    /// inner cloth is pulled flatter): two seams across each half of it
    /// (`STRIP_SEAMS` of the way from its middle to its end, RLS022's
    /// panels), taut along their length, so they show as straight ridges, a
    /// lit side toward the flash.
    fn strip_cloth(p: Vec2f, half: Vec2f, seed: u32) -> f32 {
        let shade = diffuser_shade(p, &box_corners(half), STRIP_INNER_FANS, half, &|_| f32::INFINITY, seed ^ 0x3c7);
        // The seams' ridges, `STRIP_SEAM` high over `STRIP_SEAM_WIDTH`: their
        // slope up the strip, lit from the flash behind the middle.
        let (y, a, b) = (p.y.abs(), STRIP_SEAMS[0] * half.y, STRIP_SEAMS[1] * half.y);
        let dy = if (y - a).abs() < (y - b).abs() { y - a } else { y - b };
        let slope = -2.0 * STRIP_SEAM * dy / (STRIP_SEAM_WIDTH * STRIP_SEAM_WIDTH) * gauss(dy, STRIP_SEAM_WIDTH) * p.y.signum();
        let n = vec3f(0.0, -slope, 1.0).normalize();
        let l = vec3f(p.x, p.y, DIFFUSER_FLASH_DEPTH).normalize();
        let seams = (n.dot(l) / l.z).max(0.0);
        (1.0 + STRIP_RELIEF * (shade - 1.0)) * seams
    }

    /// The box's half sizes in units of the shorter one (1 on the short side).
    /// A closed softbox's, strip's or octabox's hotspot as a share of the
    /// single-diffused one the Profoto plates show (RLS010: its middle third
    /// 2.6 x the face's median): its `diffusion`, 0 as they are,
    /// `DIFFUSION_MAX` the flattest, the Lightmap cloth faces' (their middle
    /// thirds 1.2-1.7 x), its hotspot `DIFFUSION_SPOT` of that.
    fn spot(&self) -> f32 {
        if !(self.diffusion > 0.0) {
            return 1.0;
        }
        1.0 - (1.0 - DIFFUSION_SPOT) * (self.diffusion / DIFFUSION_MAX).min(1.0)
    }

    fn aspect(&self) -> Vec2f {
        let short = self.half.x.min(self.half.y);
        vec2f(self.half.x / short, self.half.y / short)
    }

    /// An octabox: the same flash through cloth, a flatter outer zone, dark
    /// at the edge, the cloth sagging between the rods, eight seams to the
    /// corners (the rods); the inner baffle's smaller cloth in its lit frame;
    /// open, the tube (`tube`, 0.87 of the light) at the back of the eight
    /// silver segments, folded and lit (open_box.rs).
    fn octabox(&self, u: Vec2f, r: f32, fx: f32, fy: f32) -> f32 {
        // Eight sides as the shape core draws a regular polygon, a corner
        // straight up: the octagon's distance from its edge, and the cloth's
        // sag between the rods (most at a side's middle).
        let sector = std::f32::consts::TAU / 8.0;
        let a = u.x.atan2(u.y);
        let off = ((a - 0.5 * sector) / sector - ((a - 0.5 * sector) / sector).round()) * sector;
        let octagon = 1.0 - r * off.cos() / (std::f32::consts::PI / 8.0).cos();
        let sag = 0.015 * (4.0 * off).cos().powi(2);
        let inside = HOUSING + (1.0 - HOUSING) * smooth(self.rim + sag, self.rim + sag + EDGE, octagon);
        let depth = 1.0 - octagon;
        let open = smooth(0.75, 1.0, self.baffle);
        let inner = smooth(0.0, 0.7, self.baffle) * (1.0 - open);
        let spot = self.spot();
        let closed = (0.45 + 1.8 * (spot * (-3.5 * r * r).exp() + (1.0 - spot) * DIFFUSION_EVEN)) * (1.0 - 0.4 * smooth(0.86, 1.0, depth));
        let n = if self.ribs >= 3.0 { self.ribs } else { 8.0 };
        let seams = 1.0 - (0.03 + 0.25 * inner) * line(spoke_gap(u, n, 0.0), 0.7 * LINE);
        let cloth = creased(fx * 3.0, fy * 3.0, 0.04 + 0.3 * inner, self.seed) * cloud(fx * 1.5, fy * 1.5, 0.03, self.seed ^ 0x11);
        // Each segment's wall its own shade.
        let segment = ((a / std::f32::consts::TAU * n).rem_euclid(n)).floor() as i32;
        let shade = 0.8 + 0.4 * hash01(segment, 0, 0, self.seed ^ 0x5e);
        // The front diffuser's folds and seams from the tension map
        // (`diffuser_shade`: the eight rod tips pull it, the seams run
        // between them); the inner baffle's cloth keeps its own.
        let front = closed * diffuser_shade(u, &octa_corners(), OCTA_FANS, vec2f(1.0, 1.0), &|q| spoke_gap(q, n, 0.0), self.seed ^ 0x3c1);
        let mut diffused = mix(front, closed * cloth * seams, smooth(0.0, 0.15, inner));
        if inner > 0.0 {
            // The inner baffle as RLS018 shows it (`octa_inner`): the inner
            // cloth and the band of silver walls round it, folded by the
            // lining's tension map.
            let folds = self.open.as_ref().map_or(1.0, |lining| lining.folds(u));
            let (walls, inner_face) = self.octa_inner(u, depth, octagon, folds, shade, spoke_gap(u, n, 0.0));
            let inner_edge = smooth(OCTA_INNER_EDGE, OCTA_INNER_EDGE + EDGE, octagon - 0.3 * sag);
            diffused = mix(diffused, mix(walls, inner_face, inner_edge), inner);
        }
        if open <= 0.0 {
            return inside * diffused;
        }
        // Open: the silver segments as the tube lights them.
        let walls = self.open.as_ref().map_or(0.0, |lining| lining.walls(u));
        inside * mix(diffused, walls, open)
    }

    /// An octabox's inner baffle as RLS018 shows it, over the face's median
    /// (`OCTA_MEDIAN` of its mean): the inner cloth (inside the octagon
    /// distance `OCTA_INNER_EDGE` from the opening's edge, `depth` the
    /// distance out from the middle, 1 at a side) holds the flash's core,
    /// falling as e^(-t / 0.125) from 17 x at the middle (a sharper core
    /// in it) to 0.85 x at its edge, its contours rounded squares with a
    /// ray from each corner, their sides toward four of the sides' middles; its folds from its own tension map (`diffuser_shade`: the
    /// eight rods pull it at its corners), no seams, a lit hem round it. The
    /// silver walls' band round it falls from 0.92 x by the cloth to 0.6 x by
    /// the frame (the folded foil's median under its mean, `OCTA_WALLS`), each segment its own `shade`, `folds` the lining's folded
    /// foil (`OpenBox::folds`), a dark line down each rod (`seam` the
    /// distance to it) and the cloth's shadow line on the walls by its edge.
    /// The walls and the inner face.
    fn octa_inner(&self, u: Vec2f, depth: f32, octagon: f32, folds: f32, shade: f32, seam: f32) -> (f32, f32) {
        let k = 1.0 - OCTA_INNER_EDGE;
        // The core's contours: rounded squares, their sides toward four of
        // the sides' middles, a ray out of each corner.
        let (sn, cs) = std::f32::consts::FRAC_PI_8.sin_cos();
        let (x, y) = (cs * u.x - sn * u.y, sn * u.x + cs * u.y);
        let square = (x.powi(4) + y.powi(4)).sqrt().sqrt();
        let star = (2.0 * x * y / (x * x + y * y).max(1.0e-9)).powi(4);
        let t = square / (1.0 + 0.15 * star);
        // `diffusion` spreads the core into a broad dome at the same mean, as
        // a deeper box's inner cloth shows it (HDR Light Studio's Rotalux
        // Deep Octa, inner diffuser only: 2.1 x its median at the middle to
        // 1.2 x by its edge), its cube (the deeper cloth spreads it further).
        let spot = self.spot();
        let level = if spot < 1.0 {
            let keep = spot * spot * spot;
            let core = 16.0 * (-t / 0.125).exp() + 8.0 * (-t / 0.035).exp();
            OCTA_MEDIAN * (0.8 + keep * core + (1.0 - keep) * OCTA_INNER_DOME * (-t / OCTA_INNER_DOME_R).exp())
        } else {
            OCTA_MEDIAN * (0.8 + 16.0 * (-t / 0.125).exp() + 8.0 * (-t / 0.035).exp())
        };
        let pulls = octa_corners().map(|c| vec2f(c.x * k, c.y * k));
        let cloth = diffuser_shade(u, &pulls, OCTA_INNER_FANS, vec2f(k, k), &|_| f32::INFINITY, self.seed ^ 0x3c5);
        let hem = 1.0 + 0.25 * line(octagon - OCTA_INNER_EDGE - 0.008, 0.008);
        let walls_level = OCTA_WALLS * (0.92 - 1.4 * (depth - k).max(0.0));
        let shadow = 1.0 - 0.45 * line(octagon - OCTA_INNER_EDGE + 0.012, 0.012);
        let rods = 1.0 - 0.55 * line(seam, 0.01);
        (walls_level * shade * shadow * rods * folds.powf(OCTA_WALL_CONTRAST), level * cloth * hem)
    }

    /// The edge of an umbrella: the cloth runs straight from rib to rib (the
    /// `n`-gon with its corners at the ribs, taut) and sags a little between
    /// them; past it there is nothing (0).
    fn scalloped(&self, u: Vec2f, n: f32, r: f32) -> f32 {
        let edge = self.hem(u, n);
        1.0 - smooth(edge - EDGE, edge, r)
    }

    /// How far an umbrella's hem is from the hub at `u`'s angle: the cloth
    /// straight from rib to rib (the `n`-gon with its corners at the ribs,
    /// taut), sagging a little between them.
    fn hem(&self, u: Vec2f, n: f32) -> f32 {
        let sector = std::f32::consts::TAU / n;
        let a = u.x.atan2(u.y);
        let off = (a / sector - (a / sector).round()) * sector;
        let polygon = (0.5 * sector).cos() / off.cos().max(1.0e-3);
        let mut sag = (1.0 - polygon) + 0.012 * (0.5 * n * a).sin().powi(2);
        let cover = self.cover();
        if cover > 0.0 {
            // A cover pulled taut from tip to tip: its hem bows in between
            // (a white umbrella's; a parabolic's second diffuser is inside).
            sag += cover * smooth(0.06, 0.12, self.baffle) * COVER_SAG * (0.5 * n * a).cos().powi(2);
        }
        1.0 - self.rim - sag
    }

    /// A white umbrella: the strobe head and its stand in silhouette under
    /// the hub, the face brightest round the head (the flash, `from_flash`)
    /// and falling to the edge (`baffle` sets how fast: 0 the flat
    /// parabolic, 0.6 the white bounce), the ribs as thin light lines from
    /// the hub, the panels between them a little brighter down their middles
    /// and softly streaked along the radius (the plates' cloth is smooth:
    /// RLS024 and the Lightmap umbrellas show no blotches).
    fn umbrella(&self, u: Vec2f, r: f32, head: bool) -> f32 {
        let n = if self.ribs >= 3.0 { self.ribs } else { 8.0 };
        let a = u.x.atan2(u.y);
        let hot = self.umbrella_hot(u) / self.norm;
        let silhouette = head_and_stand(u, if head { 0.5 * self.plate } else { 0.0 });
        // A bounce umbrella's ribs are in front of its cloth, metal rods with
        // the cloth pulled into folds from their tips; a parabolic's (baffle
        // near 0) are behind its front diffuser, which shows neither: its
        // cloth is stretched over the rib tips instead (`front_cloth`).
        let metal = smooth(0.1, 0.4, self.baffle);
        let mut ribs = umbrella_ribs(u, n, (0.2 + 0.2 * self.baffle) * metal, metal);
        let drop = if self.plate < 0.02 { 0.0 } else { HEAD_DROP * 0.5 * self.plate };
        let mut folds = umbrella_folds(u, n, 1.0 - self.rim, vec2f(0.0, -drop), metal, self.seed ^ 0x6b1);
        // A diffuser over the face (`cover`) hides the ribs and their folds
        // and calms the parabolic's front cloth; its own folds are its
        // tension fans from the rib tips (`cover_folds`).
        let cover = self.cover();
        if cover > 0.0 {
            ribs = mix(ribs, 1.0, cover);
            let gathered = 1.0 - smooth(0.06, 0.12, self.baffle);
            folds = mix(folds, 1.0, cover) * cover_folds(u, n, 1.0 - self.rim, cover, gathered, self.seed ^ 0x6c5);
        }
        let front = front_cloth(u, n, self.hem(u, n), 1.0 - smooth(0.06, 0.12, self.baffle), 1.0 - cover, self.seed ^ 0x7c3);
        // The cloth between the ribs: a little brighter down each panel's middle, streaked along it.
        let panels = 1.0 + 0.05 * self.baffle * (0.5 * n * a).sin().powi(2) * 2.0 - 0.05 * self.baffle;
        let (sa, ca) = a.sin_cos();
        let streaks = 1.0 + 0.07 * (2.0 * fbm3(ca * 3.0, sa * 3.0, r * 1.5, 3, self.seed ^ 0x21) - 1.0) * smooth(0.05, 0.3, r);
        // The head and its stand are in front of the ribs and the cloth.
        self.scalloped(u, n, r) * hot * silhouette * behind_head(ribs * folds * front * panels * streaks, silhouette)
    }

    /// A white umbrella's hot zone (`hotspot` of the distance from the
    /// flash, and the glow hugging the head that RLS024 blows out), before
    /// `norm`.
    fn umbrella_hot(&self, u: Vec2f) -> f32 {
        let rf = self.from_flash(u);
        let hot = hotspot(rf, 5.3 * self.baffle, 0.08) + self.baffle * gauss(rf, 0.22);
        // A parabolic with its second diffuser (RLS027, over its median):
        // flat 1.08 in the middle, 0.89 half way out, the cloth gathered by
        // the hem in a bright ring (1.5 x at 0.8 of the radius), where it
        // bulges out to the bowl's lit walls.
        let parabolic = self.cover() * (1.0 - smooth(0.06, 0.12, self.baffle));
        if parabolic <= 0.0 {
            // A white umbrella's cover (RLS026) glows round the head, where
            // the bounce's hot zone lights it.
            let white = self.cover() * smooth(0.06, 0.12, self.baffle);
            return if white > 0.0 { hot + white * COVER_GLOW * gauss(rf, COVER_GLOW_R) } else { hot };
        }
        let r = u.length();
        mix(hot, 1.08 - 0.19 * smooth(0.0, 0.5, r) + 0.7 * gauss(r - 0.77, 0.11), parabolic)
    }

    /// How much a diffuser over an umbrella's face hides (`diffusion`, 0
    /// none, 1 at `DIFFUSION_MAX`): a white umbrella's front cover (RLS026),
    /// a parabolic's second diffuser (RLS027).
    fn cover(&self) -> f32 {
        if self.kind != ModifierKind::Umbrella || !(self.diffusion > 0.0) {
            return 0.0;
        }
        (self.diffusion / DIFFUSION_MAX).min(1.0)
    }

    /// The distance from an umbrella's flash, the strobe head's middle
    /// `HEAD_DROP` head radii under the hub (at the hub without a head):
    /// a white umbrella's hot zone is round it (a silver bowl's is round the
    /// hub, where it mirrors the flash).
    fn from_flash(&self, u: Vec2f) -> f32 {
        let head = 0.5 * self.plate;
        let drop = if head < 0.01 { 0.0 } else { HEAD_DROP * head };
        vec2f(u.x, u.y + drop).length()
    }

    /// The back of an umbrella's cloth at `q` (face coordinates, as
    /// `factor`), seen from behind by a light's 3D model (light_model.rs): a
    /// white umbrella's translucent cloth lets `TRANSLUCENT_BACK` of what
    /// lights it through, so its back glows with its front's hot zone and
    /// folds, the ribs and the hub in front of it shadows on it (thin dark
    /// lines), the strobe head behind it unseen. None for every other face
    /// (a silver umbrella's black back is opaque).
    pub fn back(&self, q: Vec2f) -> Option<f32> {
        if matches!(self.kind, ModifierKind::Reflector | ModifierKind::Gradient) {
            return self.panel_back(q);
        }
        if self.kind != ModifierKind::Umbrella || !q.x.is_finite() || !q.y.is_finite() {
            return None;
        }
        let u = vec2f(q.x / self.half.x, q.y / self.half.y);
        let r = u.length();
        let n = if self.ribs >= 3.0 { self.ribs } else { 8.0 };
        let hot = self.umbrella_hot(u) / self.norm;
        let drop = if self.plate < 0.02 { 0.0 } else { HEAD_DROP * 0.5 * self.plate };
        let folds = umbrella_folds(u, n, 1.0 - self.rim, vec2f(0.0, -drop), smooth(0.1, 0.4, self.baffle), self.seed ^ 0x6b1);
        Some(TRANSLUCENT_BACK * self.scalloped(u, n, r) * hot * folds * rib_lines(u, n, 0.5))
    }

    /// The back of a panel's cloth at `q` seen from behind by its 3D model
    /// (light_model_panel.rs): a reflector's plain black fabric, its front's
    /// folds lit by the room (`Reflector::back`); a gradient's scrim lets
    /// `TRANSLUCENT_BACK` of what lights it through, so its back glows with
    /// its front's ramp.
    fn panel_back(&self, q: Vec2f) -> Option<f32> {
        if !q.x.is_finite() || !q.y.is_finite() {
            return None;
        }
        match self.kind {
            ModifierKind::Reflector => self.reflector.as_ref().map(|p| p.back(vec2f(q.x / self.half.x, q.y / self.half.y))),
            ModifierKind::Gradient => Some(TRANSLUCENT_BACK * self.factor(q)),
            _ => None,
        }
    }

    /// The face's tangent half extents (a panel's 3D model lays its
    /// diffuser out as the face does, light_model_panel.rs).
    pub fn half(&self) -> Vec2f {
        self.half
    }

    /// A reflector's panel (a 3D model mirrors its source with the view,
    /// light_model_panel.rs `Sheen`); None for every other kind.
    pub fn reflector(&self) -> Option<&Reflector> {
        self.reflector.as_ref()
    }

    /// How far an umbrella's hem is from its hub at the angle `a` (radians
    /// clockwise from straight up, as the face's `atan2(x, y)`), box units:
    /// the outline a light's 3D model gives its bowl (light_model.rs). None
    /// for every other kind.
    pub fn outline(&self, a: f32) -> Option<f32> {
        match self.kind {
            ModifierKind::Umbrella => Some(self.hem(vec2f(a.sin(), a.cos()), if self.ribs >= 3.0 { self.ribs } else { 8.0 })),
            ModifierKind::SilverUmbrella => {
                let n = if self.ribs >= 3.0 { self.ribs } else { 16.0 };
                if self.deep.is_some() {
                    // The deep bowl's polygon, its corners on the ribs at radius 1.
                    let sector = std::f32::consts::TAU / n;
                    let off = (a / sector - (a / sector).round()) * sector;
                    Some((0.5 * sector).cos() / off.cos().max(1.0e-3))
                } else {
                    Some(self.hem(vec2f(a.sin(), a.cos()), n))
                }
            }
            _ => None,
        }
    }

    /// A silver bowl's hot zone at `r` box units from its hub, smooth (its
    /// profile, `silver_profile_at`, before `norm`): where a light's 3D model
    /// (light_model.rs) reads how far the flash's image moved with the view.
    /// A bare beauty dish's ring round its plate (`beauty_raw`'s, without its
    /// lip and zones) and a reflector's ring of the head round a bare head
    /// (`bowl_envelope`) likewise. None for every other face.
    pub fn mirror_envelope(&self, r: f32) -> Option<f32> {
        match self.kind {
            ModifierKind::SilverUmbrella => Some(Self::silver_profile_at(r, self.silver_depth(), 0.0)),
            ModifierKind::BeautyDish if self.baffle <= 0.5 => Some(0.8 + 0.9 * (-(r - self.plate).max(0.0) / BEAUTY_RING).exp()),
            ModifierKind::Bulb if self.spot_bulb() => Some(0.6 + 2.4 * smooth(0.64, 0.88, r)),
            ModifierKind::Bulb if self.plate.max(0.15) < 0.36 => Some(self.bowl_envelope(r)),
            _ => None,
        }
    }

    /// A reflector's mirrored ring of the head (`reflector_bowl`), smooth:
    /// a Zoom's at 0.85, a Widezoom's satin at 0.71 and its second by the
    /// lip, a hammered or pebbled bowl's at its focus; over a floor.
    fn bowl_envelope(&self, r: f32) -> f32 {
        let ring = |at: f32, inner: f32, outer: f32| {
            let x = (r - at) / if r < at { inner } else { outer };
            (-x * x).exp()
        };
        if self.ribs >= 3.0 {
            return 0.1 + ring(0.85, 0.08, 0.3);
        }
        let satin = smooth(0.0, 0.4, self.diffusion);
        let deep = 1.0 - smooth(0.17, 0.24, self.plate.max(0.15));
        let hammered = 0.15 + ring(mix(0.55, 1.0, deep), 0.15, 0.15);
        mix(hammered, 0.1 + ring(0.71, 0.15, 0.24) + 0.25 * ring(0.9, 0.06, 0.08), satin)
    }

    /// How deep a silver bowl is, 0 at `baffle` 0.3 (shallow) to 1 at 1.
    fn silver_depth(&self) -> f32 {
        ((self.baffle - 0.3) / 0.7).max(0.0).min(1.0)
    }

    /// A silver bowl's hot zone over the radius, in its plate's median units:
    /// a shallow bowl (`t` 0) a ring round the head peaking 2.65 x at a
    /// quarter of the radius, 1.9 at 0.12, 1.8 half way, 0.77 at three
    /// quarters and 0.53 at 0.9 (RLS025's rings: 1.4, 2.9, 1.7, 0.86, 0.53); a deep one (`t` 1) the flash's mirror
    /// image, a plateau 70 x out to 0.4 of the radius, a cliff to 0.55, and a
    /// flat outer zone at 0.8 (the EV ladder's umbrella: its blown disc holds
    /// the plateau and grows past the cliff only stops later). Mixed by `t`
    /// in stops (shallow^(1 - t) x deep^t): the flash's image brightens a
    /// stop at a time as the bowl deepens, so a bowl a little deeper than
    /// shallow keeps RLS025's ring instead of a blown disc of the deep
    /// plateau's linear share (69 x t).
    /// The deep bowl's cliff moves out by `reach` (box units): the blown zone
    /// runs further down each panel's middle than at the ribs, a star of as
    /// many long tapered points as ribs, its edge ragged. The mean (`norm`)
    /// is taken at reach 0, which the star's points and dents balance.
    fn silver_profile_at(r: f32, t: f32, reach: f32) -> f32 {
        let ring = r - 0.25;
        let shallow = 0.45 + 2.2 * (-(ring / if ring < 0.0 { 0.2 } else { 0.36 }).powi(2)).exp();
        let deep = 0.8 + 69.0 * (1.0 - smooth(0.33 + reach, 0.55 + reach, r));
        shallow.powf(1.0 - t) * deep.powf(t)
    }

    /// A silver umbrella: the flash mirrored by the bowl (`silver_profile_at`
    /// round the hub, at mean 1; `baffle` 0.3 a shallow bowl, 1 a deep
    /// one), streaked along the panels of a deep bowl (bright down each
    /// panel's middle, dark at the ribs, fading with the hot zone); its
    /// fabric the deep bowl's (`DeepSilver::fabric`: curved gores, ribs and
    /// the folds they pull in, seams, woven foil and crinkle, as RLS025's);
    /// the head and its stand in front.
    fn silver_umbrella(&self, u: Vec2f, r: f32, head: bool) -> f32 {
        let n = if self.ribs >= 3.0 { self.ribs } else { 16.0 };
        let a = u.x.atan2(u.y);
        let t = self.silver_depth();
        let panel = (0.5 * n * a).sin().powi(2);
        // Seen along the shaft the bowl mirrors the flash round its axis, the
        // hub (RLS025's rings are even round it, as the deep bowl's are), not
        // round the head below it.
        let rf = r;
        // The blown zone's edge: 0.1 further out down a panel's middle than
        // at a rib (a star of long tapered points), and ragged.
        let ragged = 0.03 * t * (2.0 * fbm2(a * 3.0, r * 5.0, 2, self.seed ^ 0x9a) - 1.0);
        let hot = Self::silver_profile_at(rf, t, 0.05 * t * (panel * panel - 0.375) + ragged) / self.norm;
        // Past the blown disc the flash's streaks run on down each panel's
        // middle, fading over the outer zone (a deep bowl's; mean about 0.4
        // over the outer ring, taken from the outer zone's own level).
        let spill = t * t * 9.0 * panel.powi(6) * (-(rf - 0.5).max(0.0) / 0.16).exp() * smooth(0.4, 0.5, rf) / self.norm;
        let along = fbm2(r * 6.0, (a / std::f32::consts::TAU * n).floor() * 3.7 + 0.5, 2, self.seed ^ 0x44);
        // Mean 1 over the ring of panels: (0.05 + 0.95 x 3/8) x (0.6 + 0.8 x 0.5).
        let streaked = (0.05 + 0.95 * panel * panel * panel * panel) * (0.6 + 0.8 * along) / 0.406;
        // The streaks show where the blown disc ends: in it everything is white.
        let streaks = mix(1.0, streaked, t * smooth(0.3, 0.45, r) * (1.0 - smooth(0.6, 0.9, r)));
        // The fabric: the deep bowl's dull fabric (its curved gores, a trough
        // and a sheen band beside each rib, the folds the ribs pull in, the
        // seams down the panels' middles with their rumples, the woven foil
        // and its crinkle; mean 1), lit by the shallow bowl's ring.
        let fabric = self.shallow.as_ref().map_or(1.0, |foil| foil.fabric(u));
        // Where the bowl mirrors the flash its light runs in rays beside every
        // rib and seam, where the cloth bends toward the flash (RLS025): `line`
        // 0 at a rib or seam, 1 half way between; the rays' mean over the
        // angle is 1 (exp(-g / w) averages w (1 - e^(-1 / w))).
        const RAY_W: f32 = 0.12;
        let lines = 2.0 * n;
        let g = 1.0 - 2.0 * ((a / std::f32::consts::TAU * lines).fract().abs() - 0.5).abs();
        let ray_mean = RAY_W * (1.0 - (-1.0 / RAY_W).exp());
        let rays = 1.0 + 1.6 * ((-g / RAY_W).exp() - ray_mean) * (1.0 - smooth(0.35, 0.7, r)) * smooth(0.03, 0.1, r);
        let silhouette = head_and_stand(u, if head { 0.5 * self.plate } else { 0.0 });
        let hub = rib_lines(u, n, 0.0);
        // The head and its stand are in front of the spokes, the hub and the
        // foil with its streaks and rays.
        let spokes = metal_spokes(u, n);
        self.scalloped(u, n, r) * (hot * behind_head(streaks * rays, silhouette) + spill) * behind_head(fabric * spokes * hub, silhouette) * silhouette
    }

    /// A beauty dish (`beauty_raw`) over its mean, so the face sits at mean
    /// 1. Its housing is drawn in `beauty_raw` (the lip's outer face, not a
    /// dark band), so there is no housing factor round it.
    fn beauty_dish(&self, u: Vec2f, r: f32) -> f32 {
        self.beauty_raw(u, r) / self.norm
    }

    /// A beauty dish as RLS001 reads over its median by radius: the dark
    /// centre plate, three clips lit on its rim; the bowl 1.4 x just past it
    /// (the flash's image round the deflector, over `BEAUTY_RING`), falling
    /// to 0.84 half way out, a step up where its inner zone ends, 0.21 past
    /// the plate, and the rolled lip's bright band (2.2 x at 0.86);
    /// the roll a thin bright line (3 x) and a dark dip just inside the
    /// housing, and the housing (`rim`) the lip's outer face: a thin grey
    /// ring at 0.2 of the mean, lit on one side, to a crisp edge (RLS001 at
    /// 1:1 shows no dark band round the dish). Hammered silver (fine
    /// dimples mostly by the ring), broad bright and dark patches (the
    /// Bron's, to 0.35 and 1.65 of the bowl). With its sock (`baffle`) the
    /// sock's own plate, RLS002: the Softlight's front diffuser over its
    /// matte white bowl (`white_sock`); a bare dish is silver.
    fn beauty_raw(&self, u: Vec2f, r: f32) -> f32 {
        self.beauty_with(u, r, true)
    }

    /// `beauty_raw`; `with_plate` false (a bare dish's 3D model, which draws
    /// the plate and its clips as geometry) leaves them out: the bowl runs on
    /// in under the plate at its ring's level, the flash's image round it,
    /// to the head's dark mount in its throat (`DISH_THROAT` in
    /// light_model_head.rs). A dish in its sock keeps its face.
    fn beauty_with(&self, u: Vec2f, r: f32, with_plate: bool) -> f32 {
        let plate_r = self.plate;
        let with_plate = with_plate || self.baffle > 0.5;
        let plate = if with_plate { smooth(plate_r, plate_r + EDGE, r) } else { 1.0 };
        let past = (r - plate_r).max(0.0);
        let ring = (-past / BEAUTY_RING).exp();
        // RLS001's bowl, over its median by radius: 1.4 just past the plate
        // falling to 0.84 at 0.5; a step up to 1.26 where its inner zone
        // ends, 0.21 past the plate, falling again to 0.81 at 0.7; the
        // rolled lip's bright band, 2.2 at 0.86; then 0.8 at 0.94.
        let at = plate_r + 0.21;
        let step = 0.45 * smooth(at - 0.006, at + 0.006, r) * (-(r - at).max(0.0) / 0.1).exp();
        let bowl = 0.8 + 0.9 * ring + step + 1.45 * gauss(r - 0.845, 0.04);
        // Just inside the housing the roll's thin bright line (3 x) and its dark dip.
        let edge = 1.0 - self.rim;
        let roll = (1.0 + 2.0 * line(r - edge + 0.03, 0.008)) * (1.0 - 0.7 * line(r - edge + 0.012, 0.008));
        let lip = (1.0 - 0.5 * smooth(0.92, edge, r)) * roll;
        // The housing: the lip's outer face, a thin grey ring (0.2 of the
        // mean), lit on one side, with a soft highlight along its middle.
        let a = u.x.atan2(u.y);
        let lip_face = 0.2 * (0.5 + 0.9 * smooth(-0.4, 0.9, (a - 0.8).cos()) + 0.4 * line(r - edge - 0.5 * self.rim, 0.4 * self.rim));
        let on_bowl = 1.0 - smooth(edge, edge + EDGE, r);
        // The three clips that hold the plate, seen on its rim: short lit
        // stubs over the plate's edge (RLS001's rods run behind the plate).
        let gap = spoke_gap(u, 3.0, 0.73);
        let clips = 0.25 * line(gap, 0.008) * smooth(plate_r - 0.07, plate_r - 0.05, r) * (1.0 - smooth(plate_r - 0.005, plate_r + 0.005, r));
        // The hammered bowl: fine dimples by the ring (the flash's image
        // broken up by the dents), faint outward, and broad bright and dark
        // patches, strongest by the ring.
        let facets = hammered(u.x, u.y, 70.0, 0.06 + 0.2 * ring, self.seed);
        let blotches = 1.0 + (0.12 + 0.2 * ring) * (2.0 * fbm2(u.x * 2.5, u.y * 2.5, 3, self.seed ^ 0x32) - 1.0);
        // The plate: near black (a hundredth of the face's mean), its rolled edge lit.
        let disc = 0.006 + 0.08 * gauss(r - plate_r + 0.012, 0.008) + clips;
        let mount = if with_plate { 1.0 } else { mix(0.03, 1.0, smooth(0.14, 0.15, r)) };
        let silver = mix(disc, mix(lip_face, bowl * lip * facets * blotches * mount, on_bowl), plate);
        // A white bowl (`diffusion` toward `DIFFUSION_MAX`, the HDR Light
        // Studio White Bare's matte white paint) in place of the silver.
        let bare = if self.diffusion > 0.0 { mix(silver, self.white_bowl(u, r), (self.diffusion / DIFFUSION_MAX).min(1.0)) } else { silver };
        // The sock wraps over the lip (RLS002): no housing band, its cloth
        // lit by the white bowl behind it.
        if self.baffle <= 0.0 {
            return bare;
        }
        mix(bare, self.white_sock(u, r), self.baffle)
    }

    /// A bare beauty dish with a matte white bowl, as HDR Light Studio's
    /// Beauty Dish White Bare reads over its median by radius (in shares of
    /// the plate's radius `P` and of the dish's): the deflector plate
    /// translucent white seen from the front, near black in its middle and
    /// glowing toward its edge (0.65 x at 0.88 P), a black rim ring from
    /// 0.93 P to 1.05 P; then the bowl lit by the flash the plate turns back, 5.6 x
    /// just past the ring, a half by 0.43 of the dish, falling to 0.71 at
    /// 0.8 and rising again where the bowl turns to the viewer (1.9 x at
    /// 0.96), the rolled lip's outer face a dark band (`rim`); three rods from
    /// the plate's rim to the bowl (up, and 120 degrees either side), thin
    /// and dark, a bracket on each from 0.7 to 0.8, a lit screw head where each
    /// meets the plate; the paint a faint stipple.
    fn white_bowl(&self, u: Vec2f, r: f32) -> f32 {
        let p = self.plate.max(0.05);
        let s = r / p;
        let plate = 0.006 + 0.65 * smooth(0.55, 0.88, s).powi(2);
        let ring = smooth(0.93, 0.93 + EDGE / p, s) * (1.0 - smooth(1.05, 1.05 + EDGE / p, s));
        let disc = mix(plate, 0.02, ring);
        let x = (r - 1.05 * p).max(0.0);
        let edge = 1.0 - self.rim;
        let level = 0.55 + 3.9 * (-x / 0.07).exp() + 1.9 * (-x / 0.2).exp() + 1.3 * smooth(0.84, 0.95, r) * (1.0 - 0.15 * smooth(0.97, edge, r));
        let gap = spoke_gap(u, 3.0, 0.0);
        let out = smooth(1.05 * p, 1.05 * p + EDGE, r) * (1.0 - smooth(0.84, 0.86, r));
        let sleeve = smooth(0.68, 0.7, r) * (1.0 - smooth(0.8, 0.82, r));
        let rods = 1.0 - out * (0.35 * line(gap, 0.004)).max(0.75 * sleeve * line(gap, 0.014));
        let screw = 4.0 * (1.0 - smooth(0.005, 0.008, vec2f(gap, r - 0.97 * p).length()));
        let paint = hammered(u.x, u.y, 120.0, 0.05, self.seed ^ 0x77);
        let bowl = level * rods * paint;
        let on_bowl = 1.0 - smooth(edge, edge + EDGE, r);
        let lip_face = 0.05;
        let inside = smooth(1.05 * p, 1.05 * p + EDGE, r);
        mix(disc, mix(lip_face, bowl, on_bowl), inside) + screw
    }

    /// The sock over the Softlight's matte white bowl (RLS002, the white
    /// beauty dish under its front diffuser): the bowl scatters the flash
    /// into the deflector's shadow, so the middle stays at 0.59 of the
    /// face's median (the silver bowl's sock 0.45), rising from a quarter of
    /// the radius to 1.21 at 0.6-0.66 and falling from 0.86 (0.76 at 0.9,
    /// 0.54 at 0.94); one side brighter, 8 % past the plate's shadow (the
    /// flash's tilt in the bowl, RLS002's lower right); and the cloth
    /// (`sock_shade`).
    fn white_sock(&self, u: Vec2f, r: f32) -> f32 {
        let level = (0.9 + 0.34 * smooth(0.25, 0.54, r) + 0.66 * gauss(r - 0.625, 0.25)) * (1.0 - 0.61 * smooth(0.85, 1.0, r)) * (1.0 - 0.3 * smooth(0.94, 1.0, r));
        let side = 1.0 + 0.08 * smooth(0.2, 0.5, r) * (u.x.atan2(u.y) - SOCK_LIT_SIDE).cos();
        level * side * sock_shade(u, self.seed ^ 0x5c4)
    }

    /// The mean of `raw` over the unit disc, `housed` inside the housing
    /// `rim` (a 128 x 128 grid over the box, the disc's points): what a face
    /// is divided by to sit at mean 1.
    fn disc_mean(&self, housed: bool, raw: impl Fn(&Self, Vec2f, f32) -> f32) -> f32 {
        let n = 128;
        let (mut sum, mut count) = (0.0f64, 0u32);
        for j in 0..n {
            for i in 0..n {
                let u = vec2f((i as f32 + 0.5) / n as f32 * 2.0 - 1.0, (j as f32 + 0.5) / n as f32 * 2.0 - 1.0);
                let r = u.length();
                if r < 1.0 {
                    let housing = if housed { HOUSING + (1.0 - HOUSING) * smooth(self.rim, self.rim + EDGE, 1.0 - r) } else { 1.0 };
                    sum += (housing * raw(self, u, r)) as f64;
                    count += 1;
                }
            }
        }
        ((sum / count.max(1) as f64) as f32).max(1.0e-4)
    }

    /// An LED ring light round a hole of `plate` of the radius, its housing
    /// `rim` wide (RLS035 bare, RLS036 diffused, both 0.51 to 0.99): a lip of
    /// the white ring's plastic inside and out (`RING_LIP` of the ring's
    /// width, lit by the diodes: half the panel's light by the rows, a sixth
    /// at the hole and at the outer edge, a lit line along each edge), and
    /// between them rows of
    /// diodes (`ribs` / 8 of them, six at 48), each row's diodes spaced 1.8 x
    /// its pitch and staggered half a diode from the next, so they pack as
    /// the plates' do (RLS035: rows 0.06 of the radius apart, 0.11 along a
    /// row, discs 0.45 of the pitch round, a third of the ring lit at its
    /// clip). Bare, each diode a sharp disc 13 x the panel between, its glow
    /// round it on the plastic; `baffle`
    /// lays a diffuser over them that leaves them showing plainly through,
    /// each a disc a little larger with a soft edge (RLS036's 0.55 of the
    /// pitch round, half the ring at its clip), the panel brighter toward the hole
    /// (0.51 of a dot there, 0.09 at the outer edge: RLS036's gaps read 0.9
    /// of its mean at the inner rows, 0.2-0.45 at the outer ones), each row
    /// at the same mean, the inner rows brighter than the outer, from 1.25
    /// to 0.75 x across the ring (RLS036: 1.37 at the hole to 1.0 at 0.9 of
    /// the radius). The face at mean 1 (`ring_mean`).
    fn ring_light(&self, u: Vec2f, r: f32) -> f32 {
        let hole = self.plate.min(0.9);
        if r < hole || 1.0 - self.rim <= hole + 0.02 {
            return HOUSING;
        }
        self.ring_raw(u, r, true) / self.norm
    }

    /// A ring light's face before its mean is taken out (`ring_light`);
    /// `dots` false puts each row at its mean (`ring_mean`). `diffusion`
    /// lays a thick diffuser over it (`ring_diffused`), as an LED panel's
    /// (`led_tiles`).
    fn ring_raw(&self, u: Vec2f, r: f32, dots: bool) -> f32 {
        let rows = self.ring_rows(u, r, dots);
        if self.diffusion <= 0.0 {
            return rows;
        }
        mix(rows, self.ring_diffused(u, r, dots), smooth(0.0, 0.1, self.diffusion))
    }

    /// A ring light behind a thick diffuser (`diffusion`; the area map's
    /// LED_Ring): each diode a soft glow on evenly lit plastic, no dark lips.
    /// The rows (laid out as `ring_rows` lays them) light the plastic across
    /// the ring as a gaussian each, `RING_DIFFUSER_SPREAD` of the ring's
    /// width wide and more as the diffuser thickens, so it falls to about
    /// half at its edges (LED_Ring: 0.6-0.68 of the gaps' 1.19 at its rows);
    /// each diode is a round soft glow on it, a gaussian of 0.12 + 0.3 x
    /// `diffusion` of their spacing along the row, its peak falling as the
    /// glow widens (LED_Ring at 0.2: about 2.5 x the gaps at the row, round,
    /// half its peak 0.05 of the radius off the row). Not normalised; `dots`
    /// false the rows' mean alone.
    fn ring_diffused(&self, u: Vec2f, r: f32, dots: bool) -> f32 {
        use std::f32::consts::TAU;
        let hole = self.plate.min(0.9);
        let outer_r = 1.0 - self.rim;
        let w = outer_r - hole;
        let lip = RING_LIP * w;
        let (first, last) = (hole + lip, outer_r - lip);
        let rows = (self.ribs / 8.0).round().max(1.0).min(8.0);
        let (start, pitch) = if (last - first) / rows <= RING_PITCH { (first, (last - first) / rows) } else { (0.5 * (first + last) - 0.5 * rows * RING_PITCH, RING_PITCH) };
        let d = self.diffusion;
        let spread = (RING_DIFFUSER_SPREAD + 0.3 * d) * w;
        let sigma = 0.12 + 0.3 * d;
        let peak = RING_DIFFUSER_GLOW * (0.18 / sigma).powi(2);
        let a = u.x.atan2(u.y);
        let mut f = 0.0f32;
        for k in 0..rows as i32 {
            let rk = start + (k as f32 + 0.5) * pitch;
            let dr = r - rk;
            let level = (-dr * dr / (2.0 * spread * spread)).exp();
            f += level;
            let nk = (TAU * rk / (RING_ALONG * pitch)).round().max(6.0);
            let along = TAU * rk / nk;
            let s = sigma * along;
            if !dots {
                // The glows' mean round the row at r: each holds 2 pi s^2, one every `along`.
                f += peak * level * (TAU.sqrt() * s / along) * (-dr * dr / (2.0 * s * s)).exp();
            } else {
                let stagger = if k % 2 == 1 { 0.5 } else { 0.0 };
                let j = (a / TAU * nk - stagger).round();
                let mut glow = 0.0f32;
                for dj in -1..=1 {
                    let aj = (j + dj as f32 + stagger) * TAU / nk;
                    let (ex, ey) = (u.x - rk * aj.sin(), u.y - rk * aj.cos());
                    let dd = ex * ex + ey * ey;
                    glow += (-dd / (2.0 * s * s)).exp();
                }
                f += peak * level * glow;
            }
        }
        f
    }

    /// The bare or lightly diffused ring (`ring_raw` without a thick diffuser).
    fn ring_rows(&self, u: Vec2f, r: f32, dots: bool) -> f32 {
        use std::f32::consts::{PI, TAU};
        let hole = self.plate.min(0.9);
        let outer_r = 1.0 - self.rim;
        let w = outer_r - hole;
        let lip = RING_LIP * w;
        let (first, last) = (hole + lip, outer_r - lip);
        let rows = (self.ribs / 8.0).round().max(1.0).min(8.0);
        // The rows from `start`, filling the ring between its lips or, too
        // few for it, in its middle (`RING_PITCH`).
        let (start, pitch) = if (last - first) / rows <= RING_PITCH { (first, (last - first) / rows) } else { (0.5 * (first + last) - 0.5 * rows * RING_PITCH, RING_PITCH) };
        let b = self.baffle;
        // The row the point is in (the lips take the first's and the last's panel).
        let k = ((r - start) / pitch).floor().max(0.0).min(rows - 1.0);
        let rk = start + (k + 0.5) * pitch;
        let nk = (TAU * rk / (RING_ALONG * pitch)).round().max(6.0);
        let (inner, outer) = ((0.42 + 0.02 * b) * pitch, (0.48 + 0.18 * b) * pitch);
        // The panel's level across the ring, from the point's own radius so no row ends in a step.
        let tr = ((r - hole) / w).max(0.0).min(1.0);
        let gap = 0.075 - 0.02 * tr + b * (0.425 - 0.37 * tr);
        // A diffuser's panel is lit by the diodes all round: by the lips, with
        // none past them, it falls to 0.4 of that over 0.7 of a pitch.
        let near = smooth(first, first + 0.7 * pitch, r) * (1.0 - smooth(last - 0.7 * pitch, last, r));
        let lit = gap * (1.0 - 0.6 * b * (1.0 - near));
        // The row's mean: the gap plus each dot's area in its cell (TAU rk / nk
        // along, the pitch across); a disc whose edge falls smoothly from a
        // to b holds pi (a b + 0.3 (b - a)²).
        let cell = TAU * rk / nk * pitch;
        // Bare, each diode's glow on the plastic round it (RLS035): from 0.2 of
        // it at its edge, falling by e over 0.15 of the pitch; it holds
        // TAU (b s sqrt(pi) / 2 + s² / 2) of the glow's peak.
        let (glow, spread) = (0.2 * (1.0 - b), 0.15 * pitch);
        let share = ((PI * (inner * outer + 0.3 * (outer - inner).powi(2)) + glow * TAU * (outer * spread * 0.886_227 + 0.5 * spread * spread)) / cell).min(1.0);
        let mean = gap + (1.0 - gap) * share;
        // Across the ring, t 0 at the hole and 1 at its outer edge: a tilt
        // whose mean over the ring's area is 1 (the mean of t weighted by the
        // radius is (h / 2 + w / 3) / (h + w / 2), a hole h and a ring w wide).
        let t = (r - hole) / w;
        let t_mean = (0.5 * hole + w / 3.0) / (hole + 0.5 * w);
        let tilt = 1.0 + 0.5 * b * (t_mean - t);
        let panel = gap / mean * tilt;
        // The lips: darker than the panel, brightening toward the rows, a lit line at each edge.
        if r < first {
            // A diffuser's bright inner rows light their lip no more than bare ones do (RLS036's 0.03-0.3 of its mean).
            return panel * (1.0 - 0.6 * b) * (0.15 + 0.4 * smooth(hole, first, r) + 0.25 * line(r - hole - 0.006, 0.006));
        }
        if r > last {
            return panel * (0.55 - 0.4 * smooth(last, outer_r, r) + 0.25 * line(outer_r - 0.006 - r, 0.006));
        }
        if !dots {
            // Past rows too few to fill the ring, the plastic alone.
            let rows_at = if r < start || r > start + rows * pitch { 0.0 } else { share };
            return (lit + (1.0 - lit) * rows_at) / mean * tilt;
        }
        // The nearest diode of this row and, for the glow, of the rows either side.
        let a = u.x.atan2(u.y);
        let mut dot = 0.0f32;
        for dk in -1..=1 {
            let kk = k + dk as f32;
            if kk < 0.0 || kk > rows - 1.0 || (dk != 0 && glow <= 0.0) {
                continue;
            }
            let rkk = start + (kk + 0.5) * pitch;
            let nkk = (TAU * rkk / (RING_ALONG * pitch)).round().max(6.0);
            let stagger = if (kk as i32) % 2 == 1 { 0.5 } else { 0.0 };
            let aj = ((a / TAU * nkk - stagger).round() + stagger) * TAU / nkk;
            let dd = (u - vec2f(rkk * aj.sin(), rkk * aj.cos())).length();
            dot = dot.max((1.0 - smooth(inner, outer, dd)).max(glow * (-((dd - outer).max(0.0) / spread).powi(2)).exp()));
        }
        (lit + (1.0 - lit) * dot) / mean * tilt
    }

    /// The ring light's outer edge at `round` (1 - r): 1 on the face, 0 in
    /// its housing, the box's crisp edge.
    fn ring_edge(round: f32, rim: f32) -> f32 {
        smooth(rim, rim + EDGE, round)
    }

    /// The ring light's mean over the ring with its housing (a 512-point
    /// midpoint sum over the radius, weighted by it, each row at its mean):
    /// what `ring_light` takes out so its face sits at mean 1.
    fn ring_mean(&self) -> f32 {
        let hole = self.plate.min(0.9);
        if 1.0 - self.rim <= hole + 0.02 {
            return 1.0;
        }
        let housing = |inside: f32| HOUSING + (1.0 - HOUSING) * inside;
        let n = 512;
        let (mut sum, mut weight) = (0.0f32, 0.0f32);
        for i in 0..n {
            let r = hole + (1.0 - hole) * (i as f32 + 0.5) / n as f32;
            sum += housing(Self::ring_edge(1.0 - r, self.rim)) * self.ring_raw(vec2f(0.0, r), r, false) * r;
            weight += r;
        }
        (sum / weight).max(1.0e-3)
    }

    /// A fresnel spot (`fresnel_lens` over its mean, the barrel round it):
    /// the face at mean 1.
    fn fresnel(&self, u: Vec2f, r: f32) -> f32 {
        self.fresnel_lens(u, r) / self.norm + self.fresnel_barrel(u, r)
    }

    /// A fresnel's lens as the Lightmap Moles and the area map read it, in
    /// units of the lens's mean (`fresnel` takes the mean out): the lamp's
    /// image a smooth hot centre falling through the lens (10 x at the
    /// middle, 8 at 0.13 of the lens radius, 4.5 at 0.25, 2.5 at 0.34, 1 at
    /// half way, 0.34 at 0.6, 0.12 at 0.75, 0.04 at the edge: between the 2k
    /// Mole's, flatter outward, and the area map's, steeper), never under 0.3
    /// degrees across; the middle disc (0.24 of the lens) and `ribs` steps
    /// round it, narrowing outward (the n-th step's outer edge at
    /// 0.24 + 0.76 (n / ribs)^0.78 as the Moles' grooves sit), each a dark
    /// groove at its outer end with the next step's riser glinting just
    /// past it, its face a little darker outward (the Moles' 0.25 and 2.2
    /// of the ring's median); cast glass, stippled and streaked; a wire
    /// guard over it, tilted as the area map's. `baffle` focuses the image
    /// (1 spot; 0 flood: the image a tenth wider over a floor of 0.15 out to
    /// the edge, the steps soft, as the Blurred Mole). The steps, the guard and the glass are finer than a
    /// small spot's bake resolves (a 7 degree key in a 1K map is 20 texels
    /// across, and point-sampled rings put its key off by up to 16 %), so
    /// they fade out under about 14 degrees and are full from 24.
    fn fresnel_lens(&self, u: Vec2f, r: f32) -> f32 {
        let rho = r / LENS_R;
        if rho > 1.02 {
            return 0.0;
        }
        let spot = self.baffle;
        let s = 1.0 + 0.1 * (1.0 - spot);
        // The image's finest feature is never under 0.3 degrees: a 7 degree
        // key's half-texel centre at 1K put its key 1.3 % off the map.
        let least = TUBE_MIN_TAN / (LENS_R * self.half.x.min(self.half.y));
        let image = 6.8 * gauss(rho, (0.241 * s).max(least)) + 3.0 * gauss(rho, 0.37 * s) + 0.66 * (-rho / (0.332 * s)).exp() + 0.15 * (1.0 - spot);
        let fine = smooth(0.12, 0.21, self.half.x.min(self.half.y));
        // The steps: `z` counts them from the middle disc's edge outward;
        // `f` runs 0..1 across one, from its inner edge (the riser) out to
        // its groove. Its soft edges are a texel's width of the box or, out
        // of focus, a quarter of a step.
        let n = self.ribs.max(3.0);
        let t = ((rho - LENS_DISC) / (1.0 - LENS_DISC)).max(0.0);
        let z = n * t.powf(1.0 / 0.78);
        let f = z - z.floor();
        let pitch = LENS_R * (1.0 - LENS_DISC) * 0.78 / n * (z.max(0.05) / n).powf(-0.22);
        let ef = (0.5 * EDGE / pitch).min(0.3) + 0.25 * (1.0 - spot);
        let step = |x: f32| smooth(-ef, ef, x);
        let zone = if t <= 0.0 {
            // The middle disc, a thin dark line inside its edge.
            1.0 - 0.6 * line((rho - LENS_DISC + 0.012) * LENS_R, 0.01)
        } else {
            let groove = step(f - 0.72) * (1.0 - 0.5 * step(f - 1.0 + ef));
            let glint = step(f - 0.01) * (1.0 - step(f - 0.09));
            (1.25 - 0.6 * f) * (1.0 - 0.8 * groove) + 1.3 * glint
        };
        let rings = mix(1.0, zone, fine * (0.35 + 0.65 * spot));
        // Cast glass: a fine stipple (the 1k Mole's), soft mottling and
        // streaks across it (the 2k Mole's), softer out of focus.
        let (sx, sy) = (0.5 * u.x + 0.87 * u.y, -0.87 * u.x + 0.5 * u.y);
        let streaks = 1.0 + 0.1 * (2.0 * value2(sx * 3.0, sy * 28.0, self.seed ^ 0x72) - 1.0);
        let stipple = hammered(u.x, u.y, 110.0, 0.35 * (0.4 + 0.6 * spot), self.seed ^ 0x71);
        let glass = mix(1.0, stipple * streaks * cloud(u.x * 6.0, u.y * 6.0, 0.1, self.seed), fine);
        // The wire guard: thin dark wires a little bowed (the guard is
        // domed), turned 21 degrees, a cell's middle over the hot centre.
        let bow = 1.0 + 0.1 * r * r;
        let (c, sn) = (0.934f32, 0.358f32);
        let (gx, gy) = ((c * u.x - sn * u.y) * bow + 0.5 * MESH, (sn * u.x + c * u.y) * bow + 0.5 * MESH);
        let to_wire = |x: f32| 0.5 * MESH - (x.rem_euclid(MESH) - 0.5 * MESH).abs();
        let guard = 1.0 - 0.65 * fine * line(to_wire(gx), 0.005).max(line(to_wire(gy), 0.005));
        let lens = 1.0 - smooth(LENS_R - 0.012, LENS_R, r);
        image * rings * glass * guard * lens
    }

    /// A fresnel's barrel round its lens, in units of the face's mean: near
    /// black (0.004), the lens's retaining ring faintly lit with its four
    /// clips, eight slots whose far edges catch a little light, and the
    /// barrel's front lip a thin grey ring at the outer edge (`rim` sets
    /// how far in, a share of it), as the area map shows it.
    fn fresnel_barrel(&self, u: Vec2f, r: f32) -> f32 {
        use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, TAU};
        if r < LENS_R - 0.012 {
            return 0.0;
        }
        let a = u.x.atan2(u.y);
        let past = smooth(LENS_R - 0.012, LENS_R, r);
        let retainer = 0.012 * line(r - LENS_R - 0.01, 0.008);
        // Four clips on the retaining ring, at the diagonals.
        let clip_at = ((a - FRAC_PI_4) / FRAC_PI_2).round() * FRAC_PI_2 + FRAC_PI_4;
        let off = r * (a - clip_at).sin().abs();
        let clips = 0.03 * (1.0 - smooth(0.02, 0.02 + EDGE, off)) * line(r - LENS_R - 0.012, 0.025);
        // Eight slots across the barrel, their far edges lit.
        let sector = TAU / 8.0;
        let slot_at = ((a - 0.2) / sector).round() * sector + 0.2;
        let along = (a - slot_at).abs();
        let slots = 0.015 * line(r - 0.875, 0.004) * (1.0 - smooth(0.09, 0.11, along));
        let lip = 0.12 * line(r - (1.0 - 0.15 * self.rim - 0.01), 0.006) + 0.008 * gauss(r - 0.99, 0.02);
        BARREL * past + retainer + clips + slots + lip
    }

    /// A bare head (`bulb_raw`) over its mean, so the face sits at mean 1.
    fn bulb(&self, u: Vec2f, r: f32) -> f32 {
        self.bulb_raw(u, r) / self.norm
    }

    /// A bare flash head as the plates show it (RLS007 tube lit, RLS009
    /// modelling lamp lit), laid out in units of the flash tube's ring,
    /// `plate` of the radius (`D`), in the brightest part's units (`bulb`
    /// takes the mean out): its middle (`bare_head`: the lamp, the glass
    /// tube and the metal they light, to the plate's edge at 1.45 D); a
    /// black groove; the powder-coated housing to 2.2 D (a thousandth of
    /// the plasma, its bayonet notches, four screws, a lit bevel), dimmer
    /// with the lamp lit. When the head is a small share of the box
    /// (`plate` under 0.4) a reflector fills the rest (`reflector_bowl`)
    /// from 1.75 D, past the housing the bowl lights; lit, the modelling
    /// lamp is the Flash Head's big frosted globe to 1.42 D in a dark
    /// holder, the bowl from 1.68 D. The bowl's finish: hammered or
    /// pebbled by the head's size; `ribs` 3 or more, a Zoom's bowl of that
    /// many panels; `diffusion` toward 0.4, a Widezoom's satin (both
    /// unused by a bulb before, so no older file picks them).
    fn bulb_raw(&self, u: Vec2f, r: f32) -> f32 {
        self.bulb_with(u, r, true)
    }

    /// `bulb_raw`; `glass` false (a bare head's 3D model, which stands its
    /// glass in front of the face, `bulb_glass`) leaves out what it draws:
    /// the plate shows whole under the tube, the socket under the dome.
    fn bulb_with(&self, u: Vec2f, r: f32, glass: bool) -> f32 {
        use std::f32::consts::{PI, TAU};
        if self.spot_bulb() {
            return self.spot_bulb_raw(u, glass);
        }
        let d = self.plate.max(0.15);
        let s = r / d;
        let e = EDGE / d;
        let lit = self.baffle;
        let a = u.x.atan2(u.y);
        let v = vec2f(u.x / d, u.y / d);
        let seed = self.seed;
        // A disc of radius `size` (D units) at angle `at` and radius `rad`: 1 in it.
        let spot = |at: f32, rad: f32, size: f32| -> (f32, f32) {
            let c = vec2f(rad * at.sin(), rad * at.cos());
            let dist = (v - c).length();
            (1.0 - smooth(size, size + e, dist), dist)
        };
        // A screw head: brighter metal with a dark cross and a dark ring round it.
        let screw = |at: f32, rad: f32, size: f32| -> f32 {
            let (head, dist) = spot(at, rad, size);
            let c = vec2f(rad * at.sin(), rad * at.cos());
            let cross = line(v.x - c.x, 0.25 * size).max(line(v.y - c.y, 0.25 * size));
            let ring = line(dist - size, 0.35 * size);
            (1.0 + head * (5.0 - 3.0 * cross)) * (1.0 - 0.4 * ring)
        };
        let (dome, socket, tube, plate, tube_cover) = Self::bare_head(v, s, a, e, lit, seed);
        let model = if glass { BulbGlass::default() } else { bulb_glass(self.plate, lit) };
        // (the dome and the globe stand exactly over their cores, their soft edges stay on the plate)
        let lamp = if model.dome && s < DOME_R { socket } else { dome + socket };
        let (tube, plate) = if model.tube { (0.0, plate) } else { (tube, plate * (1.0 - 0.6 * tube_cover)) };
        // The groove, then the matte housing with its notches and screws.
        let groove = 0.0015 * (1.0 - 0.7 * lit) * smooth(1.45, 1.45 + e, s) * (1.0 - smooth(1.6, 1.6 + e, s));
        let notch = |at: f32| {
            let off = ((a - at + PI).rem_euclid(TAU) - PI).abs();
            1.0 - 0.7 * (1.0 - smooth(0.11, 0.13, off)) * (1.0 - smooth(1.8, 1.8 + e, s))
        };
        // The powder coat: dark charcoal inside, lighter outward, grainy.
        let coat = (1.0 + 0.18 * (2.0 * value2(v.x * 90.0, v.y * 90.0, seed ^ 0x64) - 1.0)) * (0.3 + 0.75 * smooth(1.6, 2.15, s));
        let bevel = 1.0 + 0.6 * gauss(s - 2.14, 0.04) + 1.5 * gauss(s - 1.62, 0.025);
        let fixings = screw(-0.92, 1.95, 0.05) * screw(1.17, 1.95, 0.05) * screw(3.0, 1.95, 0.05) * screw(-2.2, 1.95, 0.05);
        let housing = 0.002 * (1.0 - 0.7 * lit) * coat * bevel * notch(-1.03) * notch(0.74) * notch(2.9) * fixings * smooth(1.6, 1.6 + e, s) * (1.0 - smooth(2.2, 2.2 + e, s));
        let behind = groove + housing;
        // A reflector round a head that is a small share of the box: its cup
        // covers the plate past the tube and the housing.
        let reflector = 1.0 - smooth(0.3, 0.42, d);
        if reflector <= 0.0 {
            return lamp + tube + plate + behind;
        }
        // Lit, its modelling lamp is the Flash Head's big frosted globe (to
        // 1.42 D, over the tube and the plate): a hot middle ten times the
        // bowl falling to frosted glass at 0.8 of it, in a dark holder ring
        // (to 1.68 D) with four lit clips. Off, the bare head shows whole, its
        // plate and housing to 2.2 D (RLS006).
        let big = lit * reflector;
        let globe = if model.globe && s < GLOBE_R { 0.0 } else { (0.97 * (1.0 - smooth(0.45, 0.9, s)) + 0.083 * (1.0 + 0.35 * (1.0 - smooth(0.85, 1.3, s)))) * cloud(v.x * 3.0, v.y * 3.0, 0.06, seed ^ 0x61) * (1.0 - smooth(GLOBE_R, GLOBE_R + e, s)) };
        let clips = [-0.45f32, 0.45, -1.75, 1.75].iter().fold(0.0f32, |m, &at| m.max(spot(at, 1.56, 0.07).0));
        let holder = (0.03 + 0.06 * clips) * (1.0 - 0.4 * line(s - 1.47, 0.03)) * smooth(1.42, 1.42 + e, s) * (1.0 - smooth(1.68, 1.68 + e, s));
        let in_head = 1.0 - smooth(1.68, 1.68 + e, s);
        // The bowl starts at the holder round a globe, past the housing round
        // a bare head, a lit lip where it meets it.
        let throat = mix(1.75, 1.68, big);
        let bowl_in = smooth(throat, throat + e, s);
        let lip = 0.1 * gauss(s - throat - 0.04, 0.02);
        let bowl = (Self::reflector_bowl(u, r, d, self.ribs, self.diffusion, seed) + lip) * bowl_in;
        // Round a bare head the bowl's light falls back on its plate and housing.
        let bounce = reflector * (1.0 - big);
        mix(lamp + tube + plate * (1.0 + 1.5 * bounce), globe + holder, big * in_head) + mix(behind, bowl + 20.0 * behind * (1.0 - bowl_in) * (1.0 - big), reflector)
    }

    /// A spun reflector's bowl that mirrors the head in one broad ring, dark
    /// inside it, whatever the head's size: with `flutes` panels (3 or more)
    /// the Zoom's faceted bowl (RLS003: under a hundredth of the globe to 0.5
    /// of the radius, a grain rising to 0.03 by 0.68, the ring 0.19 at 0.85
    /// falling to 0.15 by the lip's bright bead, in concentric terraces, each panel's
    /// image sliding across it, thin seams between); else the Widezoom's
    /// satin (RLS005: 0.02 to 0.45, 0.07 at 0.54, 0.37 at 0.71, 0.2 at
    /// 0.92), the ring blurred by the satin, wider outward where the bowl
    /// flattens, fine spinning streaks and broad uneven patches over it.
    fn spun_bowl(u: Vec2f, r: f32, flutes: f32, seed: u32) -> f32 {
        use std::f32::consts::TAU;
        let a = u.x.atan2(u.y);
        // The mirrored ring: a bell steeper inside than out.
        let ring = |at: f32, inner: f32, outer: f32| {
            let x = (r - at) / if r < at { inner } else { outer };
            (-x * x).exp()
        };
        // Spun bowls stay lit to their lip, the Zoom's lip a bright bead.
        let edge = 1.0 - 0.9 * smooth(0.985, 1.0, r);
        if flutes >= 3.0 {
            // Terraces 14 to the radius, each leaning its own way (brighter
            // or darker by up to a third), a soft lit step at its edge.
            let (band, band_f) = ((r * 14.0).floor(), (r * 14.0).fract());
            let lean = hash01(band as i32, 0, 0, seed ^ 0x6d) - 0.5;
            let step = 1.0 + 0.2 * gauss(band_f - 0.1, 0.08);
            // Flat panels: across each the head's image slides, a seam between.
            let f = (a / TAU * flutes).rem_euclid(1.0);
            let panel = (0.92 + 0.16 * f) * (1.0 - 0.15 * gauss(f.min(1.0 - f) * TAU / flutes * r, 0.002));
            let grain = 1.0 + 0.5 * (2.0 * value2(u.x * 200.0, u.y * 200.0, seed ^ 0x6e) - 1.0);
            let lit = 0.19 * ring(0.85 + 0.03 * lean, 0.08, 0.3) * panel * step * (1.0 + 0.65 * lean);
            let dark = (0.003 + 0.025 * smooth(0.5, 0.68, r)) * grain;
            return (lit + dark + 0.06 * gauss(r - 0.975, 0.008)) * edge;
        }
        // The satin's spinning marks, faint; broad uneven patches and bands
        // round the bowl where its spun wall waves (RLS005: a second bright
        // band by the lip, a darker one between).
        // (Laid out on the direction, not the angle, so no seam where it wraps.)
        let dir = if r > 1.0e-6 { vec2f(u.x / r, u.y / r) } else { vec2f(0.0, 1.0) };
        let streaks = 1.0 + 0.05 * (2.0 * value2(r * 260.0 + 1.2 * dir.x, 1.2 * dir.y, seed ^ 0x6f) - 1.0);
        let patches = cloud(u.x * 1.8, u.y * 1.8, 0.3, seed ^ 0x70);
        let bands = 1.0 + 0.2 * (2.0 * value2(r * 9.0 + 0.6 * dir.x, 0.6 * dir.y, seed ^ 0x71) - 1.0);
        let lit = 0.37 * (ring(0.71 + 0.1 * (patches - 1.0), 0.15, 0.24) + 0.25 * ring(0.9, 0.06, 0.08)) * streaks * patches * bands;
        let dark = 0.004 + 0.015 * smooth(0.25, 0.45, r) * streaks;
        (lit + dark) * edge
    }

    /// A reflector's bowl round a bare head (`bulb_raw`, `plate` under 0.4),
    /// lit by the head as a mirror is: each facet of its hammered or pebbled
    /// metal shows the head where its tilt sends the head's light to the
    /// viewer. A smooth bowl would mirror the head in one ring (`focus`, a
    /// share of the lip's radius); away from it a facet must lean by
    /// `slope` per radius to catch it, so the facets thin out to bright
    /// crescents on their outer sides and the bowl darkens. A small head in
    /// a deep bowl (`d` to 0.17: the Magnum, the Widezoom) is mirrored near
    /// the lip only, its middle dark (RLS004: under a hundredth of the head
    /// to 0.64 of the radius, 0.13 at the lip), in dimples 100 across; a
    /// larger head in a shallow one (`d` from 0.24: the Bowens 18 cm bowls)
    /// lights it evenly (RLS006: a fifth of the tube's median just past the
    /// head, a tenth at the lip) through a finer crinkled pebble, patchier.
    /// Its rolled edge is dark. In the head's units. Two finishes are
    /// picked by knobs a bare head has no other use for, and set the bowl's
    /// depth themselves (`spun_bowl`): `ribs` (3 or more) a Zoom's faceted
    /// bowl of that many panels, `diffusion` (toward 0.4) a Widezoom's satin.
    fn reflector_bowl(u: Vec2f, r: f32, d: f32, flutes: f32, satin: f32, seed: u32) -> f32 {
        if flutes >= 3.0 {
            return Self::spun_bowl(u, r, flutes, seed);
        }
        let satin = smooth(0.0, 0.4, satin);
        if satin >= 1.0 {
            return Self::spun_bowl(u, r, 0.0, seed);
        }
        if satin > 0.0 {
            return mix(Self::reflector_bowl(u, r, d, 0.0, 0.0, seed), Self::spun_bowl(u, r, 0.0, seed), satin);
        }
        let deep = 1.0 - smooth(0.17, 0.24, d);
        let focus = mix(0.55, 1.0, deep);
        let slope = mix(0.9, 4.0, deep);
        let (sigma, lean) = (0.25, mix(0.55, 0.75, deep));
        let radial = if r > 1.0e-6 { vec2f(u.x / r, u.y / r) } else { vec2f(0.0, 1.0) };
        // The facet's tilt: hammered, dimples 100 across, each a spherical
        // cup leaning toward its middle (one jittered point per
        // cell); pebbled, a crinkle of ridged noise 110 across,
        // its slope by differences.
        let mut g = vec2f(0.0, 0.0);
        if deep > 0.0 {
            let (x, y) = (u.x * 50.0 + 3.1, u.y * 50.0 + 7.7);
            let (ix, iy) = (x.floor(), y.floor());
            let (mut best, mut q) = (f32::INFINITY, vec2f(0.0, 0.0));
            for j in -1..=1i32 {
                for i in -1..=1i32 {
                    let (cx, cy) = (ix as i32 + i, iy as i32 + j);
                    let px = ix + i as f32 + 0.1 + 0.8 * hash01(cx, cy, 0, seed);
                    let py = iy + j as f32 + 0.1 + 0.8 * hash01(cx, cy, 1, seed);
                    let dd = (x - px) * (x - px) + (y - py) * (y - py);
                    if dd < best {
                        best = dd;
                        q = vec2f(x - px, y - py);
                    }
                }
            }
            g = g + vec2f(-q.x, -q.y) * (deep * 2.0 * lean);
        }
        if deep < 1.0 {
            let (x, y, h) = (u.x * 55.0, u.y * 55.0, 0.02);
            let f0 = folds(x, y, seed ^ 0x69);
            let (fx, fy) = ((folds(x + h, y, seed ^ 0x69) - f0) / h, (folds(x, y + h, seed ^ 0x69) - f0) / h);
            g = g + vec2f(fx, fy) * ((1.0 - deep) * 0.25 * lean);
        }
        let (gr, gt) = (1.5 * (g.x * radial.x + g.y * radial.y), -g.x * radial.y + g.y * radial.x);
        let need = (r - focus) * slope;
        let off = (gr - need) * (gr - need) + gt * gt;
        // What a facet shows at its tilt: the head itself (`sigma`), the lit
        // bowl and head round it (a lobe 0.7 wide), a little scattered everywhere.
        let mirror = (-off / (sigma * sigma)).exp();
        let sheen = (-off / 0.49).exp();
        // A deep bowl gathers the head's light into the ring by its lip, so
        // it shines brighter there (RLS004: 0.13 of the globe at the lip;
        // RLS006: 0.23 of the tube just past the head).
        let level = (mix(0.003, 0.0002, deep) + 0.2 * mirror + 0.1 * sheen) * mix(1.0, 3.5, deep);
        level * cloud(u.x * 3.0, u.y * 3.0, mix(0.3, 0.1, deep), seed ^ 0x67) * (1.0 - 0.9 * smooth(0.955, 0.975, r))
    }

    /// A bare head's middle as the plates show it (RLS007 tube lit, RLS009
    /// modelling lamp lit), in D units (`v`, `s` = |v|, `a` the angle from
    /// up, `e` the soft edge), the plasma's peak at 1 (or the frosted dome's
    /// middle, `lit`): (the lit dome, the socket, the tube, the plate round
    /// them, how much of the plate the tube's glass covers: clear glass shows
    /// it dimmed, `bulb_with`). Every
    /// metal part is a surface the lights reach, not a level: the brushed
    /// plate (grooves along x) mirrors the tube in streaks along its
    /// brushing, so it shines above and below the tube and dims beside it;
    /// the turned ring inside the tube (grooves round it) mirrors it and the
    /// dome radially; holes, slots and screws catch it on their sides. The
    /// tube is glass `GLASS` thick round a plasma column (a bell across it,
    /// the walls a tenth), its wire wound round it, its ends round bulbs in
    /// dark sockets, a metal clip over its bottom.
    fn bare_head(v: Vec2f, s: f32, a: f32, e: f32, lit: f32, seed: u32) -> (f32, f32, f32, f32, f32) {
        use std::f32::consts::{PI, TAU};
        // The tube's glass radius, the half opening at the top (radians) and
        // its axis's height over the plate (D units).
        const GLASS: f32 = TUBE_GLASS;
        const GAP: f32 = TUBE_GAP;
        const HEIGHT: f32 = TUBE_HEIGHT;
        let flash = 1.0 - lit;
        let fill = |dist: f32, radius: f32| 1.0 - smooth(radius, radius + e, dist);
        let band = |x: f32, lo: f32, hi: f32| smooth(lo - 0.5 * e, lo + 0.5 * e, x) * (1.0 - smooth(hi - 0.5 * e, hi + 0.5 * e, x));
        let wrap = |x: f32| (x + PI).rem_euclid(TAU) - PI;
        let radial = if s > 1.0e-6 { vec2f(v.x / s, v.y / s) } else { vec2f(0.0, 1.0) };
        // A light at `(dx, dy)` from here and `h` above, seen in a metal whose
        // grooves run along `groove` (a unit vector): the anisotropic sheen
        // (the half vector's slopes across the grooves, `wide`, and along
        // them, `narrow`), and the diffuse share, over the distance squared.
        let lobe = |dx: f32, dy: f32, h: f32, groove: Vec2f, narrow: f32, wide: f32| -> (f32, f32) {
            let rr = (dx * dx + dy * dy + h * h).sqrt().max(1.0e-4);
            let (sx, sy) = (dx / (h + rr), dy / (h + rr));
            let along = sx * groove.x + sy * groove.y;
            let across = -sx * groove.y + sy * groove.x;
            let spec = (-(along * along) / (narrow * narrow) - (across * across) / (wide * wide)).exp();
            (spec / (rr * rr), h / (rr * rr * rr))
        };
        // The tube's light on a metal at `v` (13 points of the ring round the
        // nearest, spaced by the distance) and the dome's (8 points round its
        // rim, 0.35 D up): (sheen, diffuse).
        let lights = |groove: Vec2f, narrow: f32, wide: f32| -> (f32, f32) {
            let (mut spec, mut diff) = (0.0f32, 0.0f32);
            if flash > 0.0 {
                let step = 0.45 * (s - 1.0).abs().max(0.12);
                for k in -6..=6 {
                    let t = wrap(a + k as f32 * step);
                    if t.abs() < GAP {
                        continue;
                    }
                    let (sp, df) = lobe(t.sin() - v.x, t.cos() - v.y, HEIGHT, groove, narrow, wide);
                    spec += step * sp;
                    diff += step * df;
                }
                // The ends' bulbs glow too, and light the plate between the sockets.
                for side in [-1.0f32, 1.0] {
                    let (sp, df) = lobe(side * GAP.sin() - v.x, GAP.cos() - v.y, HEIGHT, groove, narrow, wide);
                    spec += 0.25 * sp;
                    diff += 0.25 * df;
                }
                spec *= flash;
                diff *= flash;
            }
            if lit > 0.0 {
                for k in 0..8 {
                    let t = k as f32 * (TAU / 8.0) + 0.2;
                    let (sp, df) = lobe(0.45 * t.sin() - v.x, 0.45 * t.cos() - v.y, 0.35, groove, narrow, wide);
                    spec += lit * (TAU / 8.0) * 0.45 * 0.4 * sp;
                    diff += lit * (TAU / 8.0) * 0.45 * 0.4 * df;
                }
            }
            (spec, diff)
        };
        // The modelling lamp: lit, its frosted dome to 0.66 D, a hot middle
        // over a plateau falling to 0.35 at its edge (RLS009: 0.55 of the
        // middle at 0.3 D, 0.45 at 0.45 D, 0.35 at 0.6 D).
        let dome = fill(s, DOME_R);
        let glow = (0.33 + 0.25 * (1.0 - smooth(0.2, 0.65, s)) + 0.42 * gauss(s, 0.18)) * cloud(v.x * 3.0, v.y * 3.0, 0.06, seed ^ 0x61) * (1.0 - 0.3 * gauss(s - 0.65, 0.02));
        // Off, the socket shows: a white ceramic collar (0.52-0.64 D) the tube
        // lights brightest at its outer lip (RLS007: 0.14 of the plasma there,
        // 0.05 at its inside), round dark fittings (a bracket, contacts, screws).
        let collar = (0.06 + 0.12 * smooth(0.52, 0.63, s)) * band(s, 0.52, 0.64) * (1.0 + 0.4 * gauss(s - 0.625, 0.01));
        let fitting = |x: f32, y: f32, size: f32| {
            let q = vec2f(v.x - x, v.y - y);
            let dist = q.length();
            fill(dist, size) * (0.6 + 2.5 * gauss((q - vec2f(0.4 * size, 0.4 * size)).length(), 0.4 * size)) * (1.0 - 0.7 * (1.0 - smooth(0.04, 0.25, (q.x.abs().min(q.y.abs())) / size)))
        };
        let bracket = rounded_box(vec2f(0.7071 * (v.x + v.y), 0.7071 * (v.x - v.y)), vec2f(0.4, 0.07), 0.02);
        let contacts = rounded_box(vec2f(v.x, v.y.abs() - 0.33), vec2f(0.07, 0.05), 0.015);
        let socket = (0.0035 + 0.003 * (1.0 - smooth(-e, 0.0, bracket)) * (0.7 + 0.6 * smooth(-0.4, 0.4, v.x - v.y)) - 0.003 * (1.0 - smooth(-e, 0.0, contacts))
            + 0.02 * gauss(s - 0.5, 0.012)
            + 0.004 * (fitting(0.0, 0.0, 0.07) + fitting(-0.3, 0.17, 0.075) + fitting(0.28, -0.2, 0.075) + fitting(0.29, 0.13, 0.06)))
            * fill(s, 0.52);
        let (dome_glow, socket) = (lit * dome * glow, flash * (collar + socket));
        // The tube's centre line: the ring where |a| is past the gap, its
        // ends' round caps; `c` across the glass, -1 to 1 (outward +).
        let side = if v.x < 0.0 { -1.0 } else { 1.0 };
        let end = vec2f(side * GAP.sin(), GAP.cos());
        let (across, along) = if a.abs() >= GAP { (s - 1.0, a) } else { ((v - end).length() * (s - 1.0).signum(), side * GAP) };
        let c = across / GLASS;
        let glass = fill(c.abs() * GLASS, GLASS);
        // The plasma: a bell across the glass (RLS007: half its peak at 0.47
        // of the radius, a twentieth at the wall), patchy along it; the
        // walls glow a little brighter just inside them.
        let flicker = 1.0 + 0.1 * (2.0 * value2(along * 4.0, 0.5, seed ^ 0x71) - 1.0);
        let plasma = (-(c / 0.56) * (c / 0.56)).exp() * flicker + 0.05 * gauss(c.abs() - 0.9, 0.05);
        // The trigger wire wound round it, 11 turns, each slanted across the
        // glass: dark on the plasma, lit where it passes the walls.
        let turns = TAU / 11.0;
        let wire_at = (along + 0.17 - 0.35 * across) / turns;
        let wire_d = (wire_at - wire_at.round()).abs() * turns * s.max(0.5);
        let wire = (1.0 - smooth(0.004, 0.004 + 0.5 * e, wire_d)) * (1.0 - smooth(1.04, 1.1, c.abs())) * smooth(GAP, GAP + 0.08, along.abs());
        // Unlit, the glass mirrors the dome: a bright streak on its inner
        // side and its two edges (RLS009: 0.53 and 0.32 of the dome's middle),
        // the plate showing dark through it.
        let mirrored = (0.008 + 0.6 * gauss(c + 0.3, 0.12) + 0.3 * gauss(c + 0.4, 0.04) + 0.25 * gauss(c - 0.75, 0.05) + 0.1 * gauss(c.abs() - 0.96, 0.025)) * (1.0 + 0.25 * (2.0 * value2(along * 6.0, 1.5, seed ^ 0x72) - 1.0));
        // The clip over its bottom: a metal strap round the glass, its bend
        // shining in two bands and at its edges, a rivet in the middle.
        let clip_sdf = rounded_box(vec2f(v.x, v.y + 1.0), vec2f(0.11, 0.18), 0.03);
        let clip_in = 1.0 - smooth(-e, 0.0, clip_sdf);
        let (cx, cy) = (v.x / 0.11, (v.y + 1.0) / 0.18);
        let rivet = (v - vec2f(0.0, -1.0)).length();
        let clip = (0.15 + 0.5 * gauss(cy - 0.45, 0.15) + 0.35 * gauss(cy + 0.55, 0.12) + 0.45 * gauss(cx.abs() - 0.8, 0.1)) * (1.0 - 0.8 * gauss(clip_sdf, 0.01))
            * (1.0 - 0.6 * fill(rivet, 0.045) + 0.8 * gauss(rivet - 0.05, 0.012)) * (0.4 * flash + 0.6 * lit);
        let wire_lit = 0.12 * wire * smooth(0.6, 0.85, c.abs()) * (flash + 0.5 * lit);
        let tube = mix(glass * (flash * plasma * (1.0 - 0.6 * wire * (1.0 - smooth(0.6, 0.85, c.abs()))) + lit * mirrored) + wire_lit, clip, clip_in);
        // The metal: the brushed plate from 0.84 D to its edge at 1.45 D, the
        // turned ring inside the tube (0.67-0.84 D), each lit by `lights`.
        let (plate_spec, plate_diff) = lights(vec2f(1.0, 0.0), 0.45, 0.9);
        let rows = value2(v.x * 3.0, v.y * 110.0, seed ^ 0x62);
        let brushing = 0.55 + 0.9 * rows * rows;
        // Scratches: short bright streaks along the brushing, one row in five,
        // catching the light from more angles than the grooves.
        let (row, row_f) = ((v.y * 160.0).floor(), (v.y * 160.0).fract());
        let shift = hash01(row as i32, 0, 3, seed ^ 0x6a);
        let (cell, cell_f) = ((v.x * 6.0 + shift * 7.0).floor(), (v.x * 6.0 + shift * 7.0).fract());
        let pick = hash01(row as i32, cell as i32, 0, seed ^ 0x6b);
        let (start, length) = (0.4 * hash01(row as i32, cell as i32, 1, seed ^ 0x6b), 0.25 + 0.6 * hash01(row as i32, cell as i32, 2, seed ^ 0x6b));
        let scratch = if pick < 0.15 { (1.0 - (2.0 * row_f - 1.0).abs()).powi(2) * smooth(start, start + 0.05, cell_f) * (1.0 - smooth(start + length - 0.05, start + length, cell_f)) * (0.5 + hash01(row as i32, cell as i32, 3, seed ^ 0x6b)) } else { 0.0 };
        let plate_lit = 0.007 * plate_spec * (brushing + 1.5 * scratch) + 0.002 * plate_diff * (1.0 + 2.5 * scratch);
        let (ring_spec, ring_diff) = lights(vec2f(-radial.y, radial.x), 0.3, 1.2);
        let turned = 1.0 + 0.15 * (2.0 * value2(s * 160.0, a * 2.0, seed ^ 0x6c) - 1.0);
        let ring_lit = (0.02 * ring_spec * turned * (1.0 + 1.5 * flash * smooth(0.72, 0.86, s)) + 0.005 * ring_diff) * (1.0 + 0.8 * gauss(s - 0.675, 0.008)) + 0.06 * lit * gauss(s - 0.675, 0.01);
        // Holes through the metal: dark inside, the bevel round each lit on
        // its side away from the tube (the side that faces it).
        let away = (s - 1.0).signum();
        let hole = |at: f32, rad: f32, size: f32| -> f32 {
            let centre = vec2f(rad * at.sin(), rad * at.cos());
            let q = v - centre;
            let dist = q.length();
            let facing = if dist > 1.0e-6 { (q.x * centre.x + q.y * centre.y) / (dist * rad) * away } else { 0.0 };
            (1.0 - 0.97 * fill(dist, size)) * (1.0 + (1.0 + 2.5 * facing.max(0.0)) * gauss(dist - size - 0.012, 0.012))
        };
        let nearest = |n: f32, turn: f32| ((a + turn) / (TAU / n)).round() * (TAU / n) - turn;
        // The ring's 4 round holes (at 0.76 D) and 16 radial slots between.
        let ring_holes = hole(a.signum() * if a.abs() < 1.545 { 0.93 } else { 2.16 }, 0.76, 0.075);
        let slot_at = nearest(16.0, 0.0);
        let slot_q = vec2f(v.x * slot_at.cos() - v.y * slot_at.sin(), v.x * slot_at.sin() + v.y * slot_at.cos() - 0.765);
        let slot_sdf = rounded_box(slot_q, vec2f(0.022, 0.045), 0.02);
        let slot = (1.0 - 0.95 * (1.0 - smooth(-e, 0.0, slot_sdf))) * (1.0 + 0.8 * gauss(slot_sdf - 0.008, 0.008) * smooth(-0.02, 0.02, -slot_q.y));
        let ring = ring_lit * ring_holes * slot * band(s, 0.665, 0.9);
        // The plate's 14 holes at 1.2 D, partly under the glass, its three
        // screws at 1.3 D (domed heads, their highlight on the tube's side,
        // a dark cross, a dark gap round them) and a countersunk hole at the bottom.
        let hole_at = nearest(14.0, 0.0);
        let plate_holes = if hole_at.abs() > 0.8 { hole(hole_at, 1.2, 0.055) } else { 1.0 };
        let screw = |at: f32| -> (f32, f32) {
            let centre = vec2f(1.3 * at.sin(), 1.3 * at.cos());
            let q = v - centre;
            let dist = q.length();
            let size = 0.08;
            let shine = gauss((q + centre * (0.45 * size / 1.3)).length(), 0.35 * size);
            let cross = fill(q.x.abs(), 0.12 * size).max(fill(q.y.abs(), 0.12 * size)) * fill(dist, 0.6 * size);
            let head = fill(dist, size) * (0.04 + 0.15 * shine) * (1.0 - 0.6 * cross) * (flash + 0.4 * lit);
            (1.0 - fill(dist, size + 0.01) - 0.7 * band(dist, size, size + 0.01), head)
        };
        let quarter = (a / (PI / 2.0)).round();
        let (screws, screw_heads) = if quarter.abs() < 1.5 { screw(quarter * PI / 2.0) } else { (hole(PI, 1.27, 0.06), 0.0) };
        let (plate_in, plate_out) = (smooth(0.9, 0.9 + e, s), 1.0 - smooth(1.45, 1.45 + e, s));
        let edge = 1.0 + 2.0 * gauss(s - 1.44, 0.01);
        // The sockets the ends sit in: dark recesses round them, a lit lip.
        let recess = (v - end).length();
        let sockets = 1.0 - 0.95 * band(recess, GLASS - 0.01, 0.22) + 1.5 * gauss(recess - 0.225, 0.01);
        let plate = plate_lit * plate_holes * screws * edge * sockets * plate_in * plate_out + screw_heads + ring * sockets;
        // Clear glass shows the metal behind it, dimmed.
        let tube_cover = glass.max(clip_in);
        (dome_glow, socket, tube, plate, tube_cover)
    }

    /// A bare bulb that is a clear-glass reflector spot bulb rather than a
    /// flash head: `ribs` 1 or 2 (a flash head's reflector takes flutes from
    /// 3, a bare head none).
    pub(crate) fn spot_bulb(&self) -> bool {
        self.ribs >= 0.5 && self.ribs < 2.5
    }

    /// A clear-glass reflector spot bulb as the Spot_Bulb area map shows it,
    /// front on, in its mirror's median units (`bulb` takes the mean out),
    /// the glass's pressed rim round at the face's edge (the map's looks out
    /// of round and its middle off the rim's only because it sees the bulb a
    /// little turned; turned in 3D, ours shows the same). Seen through the
    /// clear front: the silvered bowl mirrors the
    /// filament in a broad ring from 0.625 of the radius (0.6 of its median
    /// there, 3 by 0.88, a dip to 1.2 by 0.95 and the rim's bright bead),
    /// three dark mounting tabs and a pin over it (a tenth of it); the bowl's
    /// dark neck (0.56-0.625, a twentieth); thin rings inside it; the
    /// filament, a coil bent into a C round the middle (0.41, 0.045 either
    /// side; to 2 at its middle) open to the right, its turns across it, a
    /// faint ring inside it; the capsule's front, a hot disc (to 0.215, 3.4:
    /// the map clips there). `glass` false (its 3D model, which stands the
    /// coil and the capsule in front of the bowl's neck) leaves them out: the
    /// neck's dim inside shows there.
    fn spot_bulb_raw(&self, u: Vec2f, glass: bool) -> f32 {
        use std::f32::consts::{FRAC_PI_2, PI, TAU};
        let seed = self.seed;
        let e = EDGE;
        let wrap = |x: f32| (x + PI).rem_euclid(TAU) - PI;
        let band = |x: f32, lo: f32, hi: f32| smooth(lo - 0.5 * e, lo + 0.5 * e, x) * (1.0 - smooth(hi - 0.5 * e, hi + 0.5 * e, x));
        let r = u.length();
        let rim = SPOT_RIM;
        let inside = 1.0 - smooth(rim - e, rim, r);
        if inside <= 0.0 {
            return 0.0;
        }
        let c = vec2f(u.x - SPOT_MIDDLE.x, u.y - SPOT_MIDDLE.y);
        let rc = c.length();
        let ac = c.x.atan2(c.y);
        // The mirror: rising out to 0.88, a dip inside the rim, its bead;
        // patchy, soft streaks of the bowl's facets across it.
        let rise = 0.6 + 2.4 * smooth(0.64, 0.88, rc);
        let dip = 1.0 - 0.6 * gauss(r - rim + 0.045, 0.025);
        let bead = 1.0 + 0.9 * gauss(r - rim + 0.012, 0.007);
        let side = 1.0 + 0.12 * (ac + 2.4).cos();
        let streaks = 1.0 + 0.12 * (2.0 * value2(ac * 9.0, rc * 2.0, seed ^ 0x81) - 1.0);
        let mut mirror = rise * dip * bead * side * streaks * cloud(u.x * 2.5, u.y * 2.5, 0.12, seed ^ 0x82);
        // The tabs: dark soft blobs across the ring, a tenth of it; the pin.
        let tab = |at: f32, rad: f32, half: Vec2f| {
            let q = vec2f(rc * wrap(ac - at), rc - rad);
            1.0 - 0.9 * (1.0 - smooth(-0.04, 0.035, rounded_box(q, half, 0.07)))
        };
        let pin_at = vec2f(0.74 * (-3.02f32).sin(), 0.74 * (-3.02f32).cos());
        let pin = 1.0 - 0.85 * (1.0 - smooth(0.04, 0.065, (c - pin_at).length()));
        mirror *= tab(-FRAC_PI_2, 0.8, vec2f(0.13, 0.15)) * tab(1.23, 0.8, vec2f(0.09, 0.08)) * tab(2.06, 0.8, vec2f(0.11, 0.1)) * pin;
        // The neck: dark, its outer edge a thin grey line.
        let neck = 0.05 + 0.4 * line(rc - 0.622, 0.005);
        // Inside it: dim, three thin rings by the neck; a faint ring round
        // the capsule.
        let well = 0.2 + 0.3 * line(rc - 0.48, 0.005) + 0.45 * line(rc - 0.515, 0.005) + 0.3 * line(rc - 0.54, 0.004) + 0.4 * line(rc - 0.255, 0.008);
        let in_cap = 1.0 - smooth(SPOT_CAP_R, SPOT_CAP_R + e, rc);
        // The coil: a C round the middle, open to the right, round ends; a
        // bell across it, its turns dark lines across, lit edges.
        let gap = SPOT_COIL_GAP;
        let off = wrap(ac - FRAC_PI_2);
        let (across, along) = if off.abs() >= gap {
            (rc - SPOT_COIL_R, ac)
        } else {
            let s = if off < 0.0 { -1.0 } else { 1.0 };
            let end = vec2f(SPOT_COIL_R * (FRAC_PI_2 + s * gap).sin(), SPOT_COIL_R * (FRAC_PI_2 + s * gap).cos());
            ((c - end).length(), FRAC_PI_2 + s * gap)
        };
        let cw = across.abs() / SPOT_COIL_W;
        let coil_in = 1.0 - smooth(1.0, 1.0 + e / SPOT_COIL_W, cw);
        let turns = (along * 64.0 / TAU).rem_euclid(1.0);
        let coil = (0.9 + 1.1 * (-(cw * cw) / 0.3).exp() + 0.5 * gauss(cw - 0.85, 0.08)) * (1.0 - 0.2 * gauss(turns - 0.5, 0.1));
        let inner = if glass {
            mix(well * (1.0 - in_cap) + (3.4 + 0.6 * gauss(rc, 0.1)) * in_cap, coil, coil_in)
        } else {
            well
        };
        let level = inner * (1.0 - smooth(0.555, 0.565, rc)) + neck * band(rc, 0.56, 0.626) + mirror * smooth(0.62, 0.632, rc);
        level * inside
    }

    /// An LED panel: `ribs` diodes across, as many rows as the box's aspect
    /// gives, the whole at mean 1. Bare (`baffle` 0, RLS031) each diode is
    /// its lens (`led_bare`) on the dark panel; `baffle` lays a diffuser over
    /// them, each diode a glow on it (`led_glow`: RLS033 at 0.5), and from
    /// 0.7 the glows give way to the soft dots that by 0.85 are a ghost of
    /// 2 % on a square even face (the Lightmap cell and square soft panels);
    /// from 0.8 to 1 the diffuser cloth comes in, the pillow of the diffused
    /// plate (RLS034), which shows no lattice either (`led_diffuser`, its
    /// hotspot coming in from 0.5). `plate` dims a bicolour panel's other
    /// colour, every second column of diodes, dark at 0.8 (RLS030, RLS032:
    /// one colour on); each column and each diode a little brighter or
    /// darker than the next, as a real panel's bins are. `diffusion` thickens
    /// the diffuser (`led_tiles`): the lattice blurs into soft square tiles.
    fn led_panel(&self, u: Vec2f, d: f32) -> f32 {
        let n = self.ribs.max(4.0);
        let aspect = self.half.y / self.half.x;
        let (nx, ny) = (n, (n * aspect).round().max(1.0));
        let (cx, cy) = ((u.x * 0.5 + 0.5) * nx, (u.y * 0.5 + 0.5) * ny);
        let f = vec2f(cx - cx.floor() - 0.5, cy - cy.floor() - 0.5);
        // The soft dots the lattice fades into from 0.7 (as they were from 0.85 on).
        let ghost = smooth(0.7, 0.85, self.baffle);
        let dots = |f: Vec2f| {
            let spread = 1.0 + 1.0 * self.baffle;
            let dot = 1.0 - smooth(0.2 * spread, 0.28 * spread, f.length());
            let gap = 0.001 + 0.9 * self.baffle;
            let corner = ((0.5 - f.x.abs()).powi(2) + (0.5 - f.y.abs()).powi(2)).sqrt();
            let corner_dot = 1.0 - 0.4 * gauss(corner, 0.09) * (1.0 - self.baffle);
            let lattice = gap + (1.0 - gap) * dot;
            let mean = gap + (1.0 - gap) * (0.18 * spread * spread).min(1.0);
            lattice / mean * corner_dot
        };
        let cells = if ghost >= 1.0 {
            dots(f)
        } else {
            let (i, j) = (cx.floor(), cy.floor());
            let glow = smooth(0.05, 0.45, self.baffle);
            let bare = if glow < 1.0 { self.led_bare(f, self.led_gain(i, j, nx, ny)) } else { 0.0 };
            let lit = if glow > 0.0 { self.led_glow(f, i, j, nx, ny) } else { 0.0 };
            let lattice = mix(bare, lit, glow);
            if ghost > 0.0 {
                mix(lattice, dots(f), ghost)
            } else {
                lattice
            }
        };
        let cells = if self.diffusion > 0.0 { mix(cells, Self::led_tiles(f, self.diffusion), smooth(0.0, 0.1, self.diffusion)) } else { cells };
        let diodes = mix(cells, 1.0, 0.98 * smooth(0.5, 0.85, self.baffle));
        // The diffuser's hotspot comes in from 0.5 (RLS032 and RLS033 show
        // none), as it was from 0.85 on.
        let hot = if self.baffle >= 0.85 { self.baffle } else { 0.85 * smooth(0.5, 0.85, self.baffle) };
        let soft = mix(1.0, Self::led_diffuser(u, d, self.half, self.rim, self.baffle, self.seed) / self.norm, hot);
        diodes * soft
    }

    /// A thick diffuser's tiles at `f` (-0.5..0.5 of the pitch from a diode's
    /// middle): the lattice blurred by a gaussian of 0.2 + 0.4 x `diffusion`
    /// pitches keeps only its first harmonic, (1 + a cos 2 pi x)(1 + a cos 2
    /// pi y) with a = e^(-2 pi² sigma²): soft square tiles, darkest at their
    /// corners, mean 1. The area map's LED_Panel at 0.22: 1.31 x its mean
    /// on a diode, 0.94-0.99 half way to the next, 0.59 at the corner.
    fn led_tiles(f: Vec2f, diffusion: f32) -> f32 {
        use std::f32::consts::{PI, TAU};
        let sigma = 0.2 + 0.4 * diffusion;
        let a = (-2.0 * PI * PI * sigma * sigma).exp();
        (1.0 + a * (TAU * f.x).cos()) * (1.0 + a * (TAU * f.y).cos())
    }

    /// The light of the diode in column `i`, row `j` of an `nx` x `ny` panel
    /// as a share of the panel's mean diode (0 past the panel): its colour's
    /// (`plate` dims every second column, dark at 0.8), its column's
    /// (`LED_COLUMN_SPREAD`) and its own (`LED_DIODE_SPREAD`).
    fn led_gain(&self, i: f32, j: f32, nx: f32, ny: f32) -> f32 {
        if i < 0.0 || j < 0.0 || i >= nx || j >= ny {
            return 0.0;
        }
        let (ii, jj) = (i as i32, j as i32);
        let second = 1.0 - (self.plate / 0.8).min(1.0);
        let colour = if ii % 2 == 1 { second } else { 1.0 } / (0.5 * (1.0 + second));
        let column = 1.0 + LED_COLUMN_SPREAD * (2.0 * hash01(ii, 0, 0x1ed5, self.seed) - 1.0);
        let diode = 1.0 + LED_DIODE_SPREAD * (2.0 * hash01(ii, jj, 0x1ed6, self.seed) - 1.0);
        colour * column * diode
    }

    /// A bare diode's cell at `f` (-0.5..0.5 of the pitch from its middle),
    /// lit at `w` (`led_gain`), at mean `w` (RLS031, RLS030 at 4 x): the
    /// lens a dome to 0.2 of the pitch, 0.45 of its middle at its rim, the
    /// die's seam a faint dark line down it; the collar round it lit to
    /// 0.31 by the lens's spill (a fiftieth of the lens), its edge a dark
    /// line, and the spill's glow on the panel (a lit column of RLS030 a
    /// band on it); the panel between at 1/1100 of the lens with a dark hole
    /// at each cell's corner. Unlit (`w` 0), the lens is darker than the panel, a
    /// ring of the panel's light inside it.
    fn led_bare(&self, f: Vec2f, w: f32) -> f32 {
        let r = f.length();
        let rim = 1.0 - smooth(0.17, 0.235, r);
        let lens = (1.0 - 0.55 * smooth(0.0, 0.2, r)) * rim;
        let seam = 1.0 - 0.25 * (1.0 - smooth(0.004, 0.014, f.x.abs())) * (1.0 - smooth(0.1, 0.16, r));
        let lit = lens * seam + (1.0 - rim) * (0.02 * (1.0 - smooth(0.2, 0.31, r)) + 0.004 * (-(r / 0.35).powi(2)).exp());
        let corner = ((0.5 - f.x.abs()).powi(2) + (0.5 - f.y.abs()).powi(2)).sqrt();
        let holes = 1.0 - 0.63 * (1.0 - smooth(0.1, 0.125, corner));
        let collar = 1.0 - 0.6 * line(r - 0.3, 0.012);
        let dark = 0.0009 * holes * collar * (1.0 - rim + rim * (0.5 + line(r - 0.15, 0.03)));
        (w * lit + dark) / LED_BARE_MEAN
    }

    /// A diffused diode panel's cell at `f` in column `i`, row `j`: each
    /// diode a glow on the diffuser, a core and a halo (`LED_GLOW_CORE` of
    /// its light in the core), both gaussians, widening with `baffle`, over
    /// the diffuser's own floor (`LED_GLOW_FLOOR` of the mean). RLS033 at
    /// 0.5: 3.1 x its mean on a diode, 0.77 half way to the next, 0.41 at
    /// the cell's corner, each within 5 % of a core of 0.2 of the pitch and
    /// a halo of 0.38 over a floor of 0.25. Mean 1 where all the neighbours
    /// are lit; the panel's edge, with none past it, falls to its floor.
    fn led_glow(&self, f: Vec2f, i: f32, j: f32, nx: f32, ny: f32) -> f32 {
        use std::f32::consts::PI;
        let b = self.baffle;
        let (s1, s2) = (0.12 + 0.16 * b, 0.2 + 0.36 * b);
        let (k1, k2) = (LED_GLOW_CORE / (PI * s1 * s1), (1.0 - LED_GLOW_CORE) / (PI * s2 * s2));
        let reach = if s2 < 0.42 { 1 } else { 2 };
        let mut glow = 0.0;
        for dj in -reach..=reach {
            for di in -reach..=reach {
                let w = self.led_gain(i + di as f32, j + dj as f32, nx, ny);
                if w <= 0.0 {
                    continue;
                }
                let q = (f.x - di as f32).powi(2) + (f.y - dj as f32).powi(2);
                glow += w * (k1 * (-q / (s1 * s1)).exp() + k2 * (-q / (s2 * s2)).exp());
            }
        }
        LED_GLOW_FLOOR + (1.0 - LED_GLOW_FLOOR) * glow
    }

    /// A nude COB LED in its dimpled reflector (`ribs` 0; RLS037), in the
    /// chip's units: the chip a disc of `plate` of the radius (0.22 at 0),
    /// rows of small dies under its phosphor (a third of a die's light
    /// between them, `baffle` frosting them even), a dark seam across its
    /// middle and a grey dam round the field; the holder's ring to 1.3 x the
    /// chip at 0.15 of it falling to 0.07, its inner edge a bright line; the heatsink's dark well
    /// (a thousandth, less outward) with its fan's slanted slots glinting
    /// past 1.4 x the chip; the reflector from 0.7 of the radius, lit to
    /// 1.5 % of the chip by 0.9, hammered into dimples (`dimples`), each
    /// catching the chip as a highlight, smaller toward the lip; its rolled
    /// lip a bright line, and the dark rim past it (`rim`). `lattice` false
    /// takes the dies' mean for the face's mean (`face_norm`).
    fn nude_led_raw(&self, u: Vec2f, r: f32, lattice: bool) -> f32 {
        use std::f32::consts::TAU;
        if self.diffusion > 0.0 {
            return self.single_chip_raw(u, r, lattice);
        }
        let chip = if self.plate < 0.05 { 0.22 } else { self.plate.min(0.6) };
        let lip = 1.0 - self.rim.max(0.01);
        let v = vec2f(u.x / chip, u.y / chip);
        let rc = r / chip;
        let mut out = 0.0;
        if rc < 1.0 {
            // The dies: 30 across, 21 rows, each a rounded block 0.72 x 0.4 of its cell (wide gaps between the rows).
            let field = 1.0 - smooth(0.88, 0.93, rc);
            let dies = if lattice {
                let (gx, gy) = (v.x * 15.0, v.y * 10.5);
                let (fx, fy) = (gx - gx.floor() - 0.5, gy - gy.floor() - 0.5);
                (1.0 - smooth(0.3, 0.42, fx.abs())) * (1.0 - smooth(0.15, 0.25, fy.abs()))
            } else {
                NUDE_DIE_SHARE
            };
            let seam = 1.0 - 0.65 * (1.0 - smooth(0.07, 0.1, v.y.abs()));
            let lit = mix(0.45 + 0.55 * dies, 0.45 + 0.55 * NUDE_DIE_SHARE, self.baffle.min(1.0)) * seam;
            out += mix(0.3, lit, field) * (1.0 - smooth(0.98, 1.0, rc));
        }
        // The holder's ring, its inner edge a bright line.
        out += (0.15 - 0.08 * smooth(1.0, 1.28, rc) + 0.3 * line(rc - 1.03, 0.02)) * smooth(0.98, 1.0, rc) * (1.0 - smooth(1.28, 1.36, rc));
        // The well: a thousandth falling outward, the fan's slots past the holder.
        let well = (0.0005 - 0.0003 * smooth(1.4, 0.7 / chip, rc)) * smooth(1.3, 1.36, rc);
        let a = u.x.atan2(u.y);
        let slant = a + 1.2 * (rc - 1.35);
        let k = 14.0;
        let turn = slant / TAU * k;
        let off = (turn - turn.round()) * TAU / k * r;
        // Two rings of short slots, the outer one turned half a slot.
        let ring = if rc < 1.72 { 0.0 } else { 0.5 };
        let off2 = (turn + ring - (turn + ring).round()) * TAU / k * r;
        let along = if rc < 1.72 { smooth(1.4, 1.45, rc) * (1.0 - smooth(1.62, 1.67, rc)) } else { smooth(1.76, 1.81, rc) * (1.0 - smooth(1.95, 2.0, rc)) };
        let slot = 0.001 * line(if rc < 1.72 { off } else { off2 }, 0.01) * along;
        out += well + slot;
        // The reflector: lit from 0.7, dimpled; cells about 0.02 of the radius, flatter toward the lip.
        let lit = 0.009 * smooth(0.72, 0.92, r) * (1.0 - smooth(lip - 0.015, lip - 0.005, r));
        if lit > 0.0 {
            // 0.03 of the radius round, 0.02 out, 0.014 by the lip (the integral of a 1 to 1.4 ramp from 0.75 to 0.95).
            let around = (TAU * 0.85 / 0.03).round();
            let s = (a / TAU + 0.5) * around;
            let x = ((r - 0.75) / 0.2).max(0.0).min(1.0);
            let t = (r + 0.4 * 0.2 * (x * x * x - 0.5 * x * x * x * x) + 0.4 * (r - 0.95).max(0.0)) / 0.02;
            out += lit * Self::dimples(s, t, around, self.seed);
        }
        // The rolled lip, then the dark rim.
        out + 0.012 * gauss(r - (lip - 0.008), 0.005) + 0.0001 * smooth(lip - 0.01, lip, r)
    }

    /// A single-chip LED head (`diffusion` above 0 on a nude LED; Single_Chip_LED), in the units of its dies'
    /// light: the chip a disc of `plate` of the radius (0.22 at 0), about a hundred separate dies (`SINGLE_DIE`,
    /// `SINGLE_PITCH`) in staggered rows turned `SINGLE_TURN`, the phosphor between them `SINGLE_GAP`; the holder
    /// screwed over it, its white inner lip lit by the chip, a dark groove, its grey outer ring with two screw ears
    /// diagonally across it, each a lobe round a dark screw head with a bright rim; the reflector smooth and near
    /// black, a faint glow falling outward from the holder; three thin rings of light where its lip is rolled, with
    /// glints along them. `lattice` false takes the dies' mean (`face_norm`).
    fn single_chip_raw(&self, u: Vec2f, r: f32, lattice: bool) -> f32 {
        let chip = if self.plate < 0.05 { 0.22 } else { self.plate.min(0.6) };
        let lip = 1.0 - self.rim.max(0.01);
        let rc = r / chip;
        let mut out = 0.0;
        if rc < 1.02 {
            let field = 1.0 - smooth(0.82, 0.9, rc);
            let dies = if lattice {
                let (s, c) = SINGLE_TURN.sin_cos();
                let v = vec2f(u.x * c + u.y * s, u.y * c - u.x * s) / chip;
                let j = (v.y / SINGLE_PITCH.y).round();
                let shift = 0.5 * j.rem_euclid(2.0);
                let i = (v.x / SINGLE_PITCH.x + shift).round();
                let (dx, dy) = ((v.x / SINGLE_PITCH.x + shift - i) * SINGLE_PITCH.x, (v.y / SINGLE_PITCH.y - j) * SINGLE_PITCH.y);
                let centre = vec2f((i - shift) * SINGLE_PITCH.x, j * SINGLE_PITCH.y).length();
                let die = (1.0 - smooth(SINGLE_DIE.x - 0.022, SINGLE_DIE.x + 0.022, dx.abs())) * (1.0 - smooth(SINGLE_DIE.y - 0.018, SINGLE_DIE.y + 0.018, dy.abs()));
                die * (1.0 - smooth(0.84, 0.88, centre))
            } else {
                SINGLE_DIE_SHARE * field
            };
            out += (SINGLE_GAP + (1.0 - SINGLE_GAP) * dies) * (1.0 - smooth(0.98, 1.02, rc));
        }
        // The holder: its white inner lip (a bright line at the chip's edge), the dark groove, the grey outer ring.
        let inner = (0.065 + 0.08 * gauss(rc - 1.03, 0.05)) * smooth(0.98, 1.02, rc) * (1.0 - smooth(1.14, 1.2, rc));
        let groove = 0.0028 * smooth(1.14, 1.2, rc) * (1.0 - smooth(1.5, 1.54, rc));
        let ring = 0.014 * smooth(1.5, 1.55, rc) * (1.0 - smooth(1.72, 1.76, rc));
        out += inner + groove + ring;
        // The screw ears: a lobe at 1.79 x the chip, its screw's head dark inside a bright rim, a slot across it.
        let mut ear_lit = 0.0f32;
        let mut ear_cover = 0.0f32;
        for a in [SINGLE_EAR_ANGLE, SINGLE_EAR_ANGLE + std::f32::consts::PI] {
            let c = vec2f(a.cos(), a.sin()) * (SINGLE_EAR_AT * chip);
            let e = (u - c) / chip;
            let d = e.length();
            let cover = 1.0 - smooth(0.29, 0.32, d);
            if cover <= 0.0 {
                continue;
            }
            let slot = (1.0 - smooth(0.025, 0.04, (e.x - e.y).abs().min((e.x + e.y).abs()) * std::f32::consts::FRAC_1_SQRT_2)) * (1.0 - smooth(0.12, 0.15, d));
            let head = 0.0025 * (1.0 - 0.7 * slot) + 0.02 * gauss(d - 0.19, 0.02);
            let lobe = 0.012 + 0.012 * gauss(d - 0.3, 0.015);
            ear_lit = ear_lit.max(mix(lobe, head, 1.0 - smooth(0.2, 0.22, d)) * cover);
            ear_cover = ear_cover.max(cover);
        }
        let out = mix(out, ear_lit, ear_cover);
        // The smooth reflector's glow past the holder, and the rings where its lip is rolled, glinting.
        let past = smooth(1.74, 1.8, rc) * (1.0 - smooth(lip - 0.002, lip + 0.004, r));
        let glow = (0.00003 + 0.00055 * (-(r - SINGLE_EAR_AT * chip).max(0.0) / 0.1).exp()) * past;
        let a = u.y.atan2(u.x);
        let glint = 0.4 + 1.2 * value2(a * 90.0, 3.7, self.seed ^ 0x9a1).powi(3);
        let rings = (0.001 * gauss(r - (lip - 0.14), 0.004) + 0.002 * gauss(r - (lip - 0.05), 0.004) + 0.0018 * gauss(r - (lip - 0.008), 0.005)) * glint;
        out + glow + rings
    }

    /// Hammered dimples on a reflector in cell units (`s` round, wrapping at
    /// `around`; `t` out): staggered rows of shallow caps, each catching the
    /// chip as a soft highlight round its middle (0.75-1.25 x from dimple to
    /// dimple), darker between them; no seams (RLS037's reflector: rounded
    /// highlights twice as wide as tall by the lip, the gaps half its median).
    fn dimples(s: f32, t: f32, around: f32, seed: u32) -> f32 {
        let it = t.floor();
        let mut lit = 0.0f32;
        for dj in -1..=1 {
            let ct = it + dj as f32;
            let shift = 0.5 * ct.rem_euclid(2.0);
            let is = (s - shift).floor();
            for di in -1..=1 {
                let cs = is + di as f32;
                let ws = cs.rem_euclid(around) as i32;
                let px = cs + shift + 0.5 + 0.2 * (2.0 * hash01(ws, ct as i32, 0xd1, seed) - 1.0);
                let py = ct + 0.5 + 0.2 * (2.0 * hash01(ws, ct as i32, 0xd2, seed) - 1.0);
                let q = (s - px).powi(2) + (t - py).powi(2);
                let level = 0.75 + 0.5 * hash01(ws, ct as i32, 0xd3, seed);
                lit = lit.max(level * (-q / (0.45 * 0.45)).exp());
            }
        }
        0.3 + 1.25 * lit
    }

    /// A diffused LED panel's face, before its mean (`norm`) is taken out:
    /// a gentle hotspot, 1.4 x the mean at the middle and 0.65 of that at
    /// the edge (the Lightmap soft panels, "Soft Square Grad" and its
    /// kin, 1.33-1.44 x their mean, a broad gaussian), a touch brighter at
    /// the top than at the bottom (as all of them and RLS034 are), and,
    /// with the diffuser cloth on (`baffle` 0.8 to 1), the cloth itself:
    /// a pillow with its corners rounded by 0.45 of the shorter half size
    /// (RLS034's leaves the diagonal at 0.83 of the box, the Lightmap
    /// panels' at 0.9-0.96), a soft edge, the frame round the cloth a thin
    /// dark band (`FRAME`), the panel's black (`CORNER`) past it, and a
    /// bright band just inside the edge where the cloth turns
    /// (RLS034: 1.25 x the face beside it at 0.95 of the way out), and a
    /// faint weave (RLS034's texels spread 3 % round their local mean).
    fn led_diffuser(u: Vec2f, d: f32, half: Vec2f, rim: f32, baffle: f32, seed: u32) -> f32 {
        let cloth = smooth(0.8, 1.0, baffle);
        let short = half.x.min(half.y);
        let aspect = vec2f(half.x / short, half.y / short);
        let p = vec2f(u.x * aspect.x, u.y * aspect.y);
        // The cloth sits its frame's width (0.03) inside the rim, so the frame shows.
        let inset = rim + 0.03 * cloth;
        let sdf = rounded_box(p, vec2f(aspect.x - inset, aspect.y - inset), 0.45 * cloth);
        // Past the cloth: its frame, a dark band 0.03 wide, then the panel's
        // black corners (the plate's are the studio's black).
        let past = (FRAME - CORNER) * (1.0 - smooth(0.02, 0.045, sdf)) + CORNER;
        let cover = 1.0 - smooth(-(EDGE + 0.008 * cloth), 0.0, sdf);
        let outline = mix(1.0, past + (1.0 - past) * cover, cloth);
        let band = 1.0 + 0.3 * cloth * gauss(sdf + 0.05, 0.035);
        let weave = cloud(p.x * 4.0 + 3.1, p.y * 4.0 + 7.7, 0.02 * cloth, seed ^ 0x1ed);
        // Under the cloth, the diodes' square shows through (RLS034): over
        // a halo 2.6 x brighter just past the square than at the edge, the
        // square adds `LED_CORE` of the halo's middle, its edge softened over
        // `LED_CORE_SOFT`.
        let (k, floor) = (3.2 + 0.3 * cloth, 0.6 - 0.4 * cloth);
        let square = rounded_box(p, vec2f(LED_CORE_HALF * aspect.x, LED_CORE_HALF * aspect.y), 0.05);
        let core = LED_CORE * cloth * hotspot(0.0, k, floor) * (1.0 - smooth(-LED_CORE_SOFT, LED_CORE_SOFT, square));
        outline * band * (hotspot(d, k, floor) + core) * (1.0 + 0.08 * u.y) * weave
    }

    /// A gradient panel: a scrim lit from behind (`Scrim`), at mean 1, its
    /// creases lit by its lamps (`Scrim::crease_shade`).
    fn gradient(&self, u: Vec2f) -> f32 {
        self.scrim.as_ref().map_or(1.0, |s| s.at(u) * s.crease_shade(u)) / self.norm
    }

    /// The diffused LED panel's face's mean over the box (`led_diffuser`),
    /// a 64 x 64 midpoint sum: it depends on the box's aspect, the rim and
    /// the diffuser's rounded corners.
    fn led_diffuser_mean(half: Vec2f, rim: f32, baffle: f32, seed: u32) -> f32 {
        let n = 64;
        let longest = half.x.max(half.y);
        let (ax, ay) = (half.x / longest, half.y / longest);
        let mut sum = 0.0f32;
        for j in 0..n {
            for i in 0..n {
                let u = vec2f(2.0 * (i as f32 + 0.5) / n as f32 - 1.0, 2.0 * (j as f32 + 0.5) / n as f32 - 1.0);
                let d = ((u.x * ax).powi(2) + (u.y * ay).powi(2)).sqrt();
                sum += Self::led_diffuser(u, d, half, rim, baffle, seed);
            }
        }
        (sum / (n * n) as f32).max(1.0e-3)
    }

    /// A fluorescent bank's layout on its face, in short half sizes: the
    /// along axis (the longer side; x when the face is wider than tall),
    /// the along half size, the bay's half sizes inside the housing (`rim`),
    /// the tubes (`ribs`, 4 when 0) and their pitch across, a tube's lit
    /// half length (the bay's length less its caps and sockets) and radius
    /// (its length over `FLUO_LENGTH_PER_DIAMETER`, at most
    /// `FLUO_FILL_MAX` of the pitch).
    fn fluo_layout(&self) -> FluoLayout {
        let wide = self.half.x >= self.half.y;
        let short = self.half.x.min(self.half.y);
        let long = self.half.x.max(self.half.y) / short;
        let bay = (1.0 - self.rim).max(0.05);
        let bay_long = (long - self.rim).max(0.05);
        let n = if self.ribs >= 1.0 { self.ribs } else { 4.0 };
        let pitch = 2.0 * bay / n;
        let ends = 2.0 * (FLUO_CAP + FLUO_SOCKET) / FLUO_LENGTH_PER_DIAMETER;
        let lit = 0.985 * bay_long / (1.0 + ends);
        let radius = (lit / FLUO_LENGTH_PER_DIAMETER).min(0.5 * FLUO_FILL_MAX * pitch);
        FluoLayout { wide, long, bay, bay_long, n, pitch, lit, radius }
    }

    /// A fluorescent bank's face (`fluorescent_raw`) over its mean, and its colour, at a texel `texel` short half
    /// sizes across.
    fn fluorescent(&self, u: Vec2f, texel: f32) -> (f32, Option<Vec3f>) {
        let (f, t) = self.fluorescent_raw(u, texel);
        (f / self.norm, t)
    }

    /// A fluorescent bank before its mean, in units of a tube's light: the
    /// bay inside the housing (`rim`, `FLUO_HOUSING`), bare
    /// (`fluorescent_bay`) or behind a cloth (`baffle`, `fluorescent_cloth`);
    /// and the colour a dirty cover's stains give it (`fluo_prism`).
    fn fluorescent_raw(&self, u: Vec2f, texel: f32) -> (f32, Option<Vec3f>) {
        let l = self.fluo_layout();
        let (s, t) = if l.wide { (u.x * l.long, u.y) } else { (u.y * l.long, u.x) };
        let inside = smooth(0.0, 0.004, (l.bay - t.abs()).min(l.bay_long - s.abs()));
        if inside <= 0.0 {
            return (FLUO_HOUSING, None);
        }
        let (face, tint) = match self.fluo_cover() {
            FluoCover::Open => (self.fluorescent_inner(&l, u, s, t), None),
            FluoCover::Louvre(cells) => (self.fluo_louvre(&l, s, t, cells), None),
            FluoCover::Prism(dirt) => self.fluo_prism(&l, u, s, t, dirt, texel),
        };
        (mix(FLUO_HOUSING, face, inside), tint.map(|c| Vec3f::all(1.0) + (c - Vec3f::all(1.0)) * inside))
    }

    /// What covers the tubes (`diffusion`, `FLUO_LOUVRE_TOP`): nothing at 0, a louvre, or a prismatic cover.
    fn fluo_cover(&self) -> FluoCover {
        let d = self.diffusion;
        if !(d > 0.0) {
            FluoCover::Open
        } else if d <= FLUO_LOUVRE_TOP {
            FluoCover::Louvre((FLUO_LOUVRE_CELLS * d).round().max(1.0))
        } else {
            FluoCover::Prism(((d - FLUO_PRISM_CLEAN) / (0.8 - FLUO_PRISM_CLEAN)).clamp(0.0, 1.0))
        }
    }

    /// The bank behind its cover: the bare bay, or the cloth over it (`baffle`).
    fn fluorescent_inner(&self, l: &FluoLayout, u: Vec2f, s: f32, t: f32) -> f32 {
        let cloth = smooth(0.0, 0.15, self.baffle);
        let bare = if cloth < 1.0 { self.fluorescent_bay(l, s, t) } else { 0.0 };
        if cloth > 0.0 { mix(bare, self.fluorescent_cloth(l, u, s, t), cloth) } else { bare }
    }

    /// The face's coordinates of the bank's point `s` along and `t` across (short half sizes).
    fn fluo_u(&self, l: &FluoLayout, s: f32, t: f32) -> Vec2f {
        if l.wide { vec2f(s / l.long, t) } else { vec2f(t, s / l.long) }
    }

    /// The bank seen through a louvre of `cells` cells across the bay (pnl_squared_01, pnl_lamps_04): square
    /// cells over the bay. The centre sees the face's point at tangent coordinates q along the slope q (per unit
    /// depth; in short half sizes `(s, t)` times the short half size), so the ray that enters a cell at the
    /// louvre's front runs that slope times the depth further out by its back. In a moulded plastic egg-crate
    /// (more than `FLUO_LOUVRE_SPECULAR` cells) the walls are matte and straight, a cube's depth: the ray stops on
    /// the first wall it meets, lit by the tubes behind, so toward the face's edges, seen at a slant, each cell
    /// shows its inner wall grey. A troffer's louvre (`louvre_axis`) is specular aluminium, thin blades tapered so
    /// each cell narrows toward the back: its walls show even head-on, the tubes mirrored in them. The walls'
    /// front edges are dark lines (`FLUO_LOUVRE_EDGE`, `FLUO_TROFFER_EDGE`).
    fn fluo_louvre(&self, l: &FluoLayout, s: f32, t: f32, cells: f32) -> f32 {
        let short = self.half.x.min(self.half.y);
        let pitch_t = 2.0 * l.bay / cells;
        let n_s = (2.0 * l.bay_long / pitch_t).round().max(1.0);
        let pitch_s = 2.0 * l.bay_long / n_s;
        let (a, b) = ((t + l.bay) / pitch_t, (s + l.bay_long) / pitch_s);
        let (ia, ib) = (a.floor(), b.floor());
        let (fa, fb) = (a - ia, b - ib);
        let specular = cells <= FLUO_LOUVRE_SPECULAR;
        // The front edges: a line over every wall.
        let half_edge = 0.5 * if specular { FLUO_TROFFER_EDGE } else { FLUO_LOUVRE_EDGE };
        let edge = |f: f32| 1.0 - smooth(half_edge - FLUO_LOUVRE_SOFT, half_edge + FLUO_LOUVRE_SOFT, f.min(1.0 - f));
        let front = edge(fa).max(edge(fb));
        if specular {
            // The slopes in cells across per cell of depth: the walls are FLUO_TROFFER_DEPTH cells deep.
            let k = FLUO_TROFFER_DEPTH;
            let (ga, ma) = Self::louvre_axis(fa, t * short, k, FLUO_TROFFER_TAPER);
            let (gb, mb) = Self::louvre_axis(fb, s * short * pitch_t / pitch_s, k, FLUO_TROFFER_CROSS_TAPER);
            let seen = match (ga, gb) {
                (Some(ga), Some(gb)) => {
                    let (s2, t2) = (-l.bay_long + (ib + gb) * pitch_s, -l.bay + (ia + ga) * pitch_t);
                    self.fluorescent_inner(l, self.fluo_u(l, s2, t2), s2, t2) * FLUO_LOUVRE_MIRROR.powi(ma + mb)
                }
                _ => FLUO_HOUSING,
            };
            return mix(seen, FLUO_LOUVRE_EDGE_LEVEL, front);
        }
        // A matte egg-crate: the ray stops on the first wall it meets, at the share z of the depth, the wall lit
        // by the tubes behind it, more toward its back.
        let depth = FLUO_LOUVRE_DEPTH * pitch_t;
        let (da, db) = (t * short * depth / pitch_t, s * short * depth / pitch_s);
        let hit = |f: f32, d: f32| if f + d > 1.0 { (1.0 - f) / d } else if f + d < 0.0 { -f / d } else { f32::INFINITY };
        let z = hit(fa, da).min(hit(fb, db));
        let seen = if z < 1.0 {
            let (s2, t2) = (s + z * db * pitch_s, t + z * da * pitch_t);
            FLUO_LOUVRE_WALL * (0.25 + 0.75 * z) * self.fluo_sheet_light(l, s2, t2, FLUO_LOUVRE_WALL_REACH)
        } else {
            let (s2, t2) = (s + db * pitch_s, t + da * pitch_t);
            self.fluorescent_inner(l, self.fluo_u(l, s2, t2), s2, t2)
        };
        mix(seen, FLUO_LOUVRE_EDGE_LEVEL, front)
    }

    /// One axis of a troffer's tapered mirror cell, in cells: the ray enters the front at `f` (0..1 across) with
    /// `slope` (cells across per cell of depth) and runs `depth` cells back between two mirror walls, each leaning
    /// in by `taper` of the cell over the depth. Where it leaves the back (0..1 across the front's
    /// cell) and how many times it bounced; None when the walls turn it back out of the front.
    fn louvre_axis(f: f32, slope: f32, depth: f32, taper: f32) -> (Option<f32>, i32) {
        let lean = taper / depth;
        let (mut x, mut z, mut d) = (f, 0.0f32, slope);
        let n = vec2f(1.0, -lean).normalize();
        for bounce in 0..6 {
            // The left wall x = lean z, the right x = 1 - lean z.
            let zl = if lean - d > 1.0e-6 { (x - d * z) / (lean - d) } else { f32::INFINITY };
            let zr = if lean + d > 1.0e-6 { (1.0 - x + d * z) / (lean + d) } else { f32::INFINITY };
            let (zh, left) = if zl < zr { (zl, true) } else { (zr, false) };
            if !(zh > z + 1.0e-5) || zh >= depth {
                return (Some((x + d * (depth - z)).clamp(0.0, 1.0)), bounce);
            }
            x += d * (zh - z);
            z = zh;
            // Mirrored off the wall it met: the direction (d, 1) about its normal.
            let nn = if left { n } else { vec2f(-n.x, n.y) };
            let v = vec2f(d, 1.0);
            let r = v - nn * (2.0 * (v.x * nn.x + v.y * nn.y));
            if r.y <= 1.0e-4 {
                return (None, bounce + 1);
            }
            d = r.x / r.y;
        }
        (Some((x + d * (depth - z)).clamp(0.0, 1.0)), 6)
    }

    /// The tubes' light on a sheet `depth` in front of them at `s` along and `t` across: each a line source's
    /// D / (D² + x²) across and its ends' fall along, and the reflector's image of it further back
    /// (`fluorescent_cloth`'s, without the cloth's folds), in units of a tube's light.
    fn fluo_sheet_light(&self, l: &FluoLayout, s: f32, t: f32, depth: f32) -> f32 {
        let k = (self.plate / 0.8).min(1.0);
        let image = mix(0.35, FLUO_MIRROR, k);
        let behind = depth + 2.0 * FLUO_STANDOFF * l.radius;
        let ends = |dd: f32| ((l.lit - s) / dd).atan() + ((l.lit + s) / dd).atan();
        let mut light = 0.0f32;
        for j in 0..l.n as i32 {
            let x = t - (-l.bay + (j as f32 + 0.5) * l.pitch);
            light += self.fluo_gain(j) * (depth / (depth * depth + x * x) * ends(depth) + image * behind / (behind * behind + x * x) * ends(behind));
        }
        light * l.radius / std::f32::consts::PI
    }

    /// The bank behind a prismatic cover (fluorescent_dirty): its ribs a wave across the tubes, so the ray
    /// through a rib leaves bent by its slope, up to about `FLUO_PRISM_BEND` either way, and meets the bank
    /// `FLUO_PRISM_GAP` behind that far off: each rib shows a squeezed strip of the tubes and the reflector, the
    /// face the tubes smeared across the ribs; the milky moulding's haze (`fluo_sheet_light`) under it; the bowl
    /// passing less toward its long sides, where it turns edge-on; and what water left on it (`fluo_dirt`).
    fn fluo_prism(&self, l: &FluoLayout, u: Vec2f, s: f32, t: f32, dirt: f32, texel: f32) -> (f32, Option<Vec3f>) {
        // The ribs: a moulded wave across the tubes, each rib a little unlike the next, its slope bending the ray
        // through it; where it slopes most the ray meets the bank furthest off.
        // Three rays a quarter of a rib apart: the moulding is not sharp, nor is the bank behind it in focus.
        let mut sharp = 0.0;
        for k in [-0.25f32, 0.0, 0.25] {
            let x = (t + l.bay) / FLUO_PRISM_PITCH + k;
            let rib = x.floor();
            let gain = 0.6 + 0.8 * hash01(rib as i32, 0, 0x9b, self.seed);
            let bend = FLUO_PRISM_BEND * gain * (std::f32::consts::TAU * (x - rib)).sin();
            let t2 = (t + bend * FLUO_PRISM_GAP).max(-l.bay).min(l.bay);
            sharp += self.fluorescent_inner(l, self.fluo_u(l, s, t2), s, t2) / 3.0;
        }
        let haze = self.fluo_sheet_light(l, s, t, FLUO_PRISM_GAP);
        let c = t / l.bay;
        let turn = 1.0 - FLUO_PRISM_TURN * c * c * c * c;
        let lit = mix(sharp, haze, FLUO_PRISM_HAZE) * FLUO_PRISM_PASS * turn;
        if !(dirt > 0.0) {
            return (lit, None);
        }
        // What the grime lets through: its luminance takes the level, its colour over that the tint.
        let pass = fluo_dirt_pass(self.fluo_dirt_depth(l, u, dirt, texel));
        let level = luminance(pass);
        (lit * level, Some(pass * (1.0 / level.max(1.0e-6))))
    }

    /// How deep the dust on a dirty cover lies at the face's point `u` (its optical depth through green,
    /// `FLUO_DIRT_ABSORB`), in metres of the cover (its long half `FLUO_DIRT_LONG`): a film in patches
    /// (`FLUO_DUST_PATCH` m across), thicker within `FLUO_DUST_EDGE` m of its edges and in its corners, where dust
    /// settles against the frame; fine grains, one to a cell `FLUO_GRAIN_CELL` m wide at most, more where the film is
    /// thick; and a few bigger flecks (`FLUO_FLECK_CELL`). `dirt` 0..=1 sets how much. Seen at a texel `texel` short
    /// half sizes across (`FLUO_DIRT_FOOT` at the finest): grains finer than it keep their share.
    fn fluo_dirt_depth(&self, l: &FluoLayout, u: Vec2f, dirt: f32, texel: f32) -> f32 {
        let aspect = self.aspect();
        let (ex, ey) = if l.wide { (l.bay_long, l.bay) } else { (l.bay, l.bay_long) };
        let metre = FLUO_DIRT_LONG / ex.max(ey);
        let (x, y) = (u.x * aspect.x * metre, u.y * aspect.y * metre);
        let foot = FLUO_DIRT_FOOT.max(texel * metre);
        let seed = self.seed ^ 0x0d1e;
        // The film, in patches.
        let cloud = smooth(-0.3, 0.35, fbm(x, y, FLUO_DUST_PATCH, 4, foot, seed ^ 1));
        let film = 0.15 + 0.85 * cloud;
        // Thicker along the edges, most in the corners.
        let (dx, dy) = (ex * metre - x.abs(), ey * metre - y.abs());
        let edge = (1.0 - smooth(0.0, FLUO_DUST_EDGE, dx.min(dy))) * (0.6 + 0.4 * smooth(-0.3, 0.3, fbm(x, y, 0.05, 3, foot, seed ^ 2)));
        let corner = (1.0 - smooth(0.0, FLUO_DUST_EDGE, dx.max(dy))) * 0.6;
        let mut depth = FLUO_DUST * dirt * (film + FLUO_DUST_EDGE_MORE * (edge + corner));
        // Grains and flecks: a round speck to a cell at most, there by its hash, faded to its share past the texel.
        let specks = |cell: f32, share: f32, r0: f32, r1: f32, strength: f32, salt: i32| {
            let k = 1.0 - smooth(0.25 * cell, 0.5 * cell, foot);
            let mean = share * std::f32::consts::PI * (0.5 * (r0 + r1)).powi(2) / (cell * cell);
            let mut c = 0.0f32;
            if k > 0.0 {
                let (ci, cj) = ((x / cell).floor() as i32, (y / cell).floor() as i32);
                for j in cj - 1..=cj + 1 {
                    for i in ci - 1..=ci + 1 {
                        let h = |n: i32| hash01(i, j, salt + n, seed);
                        if h(1) > share {
                            continue;
                        }
                        let (sx, sy) = ((i as f32 + h(2)) * cell, (j as f32 + h(3)) * cell);
                        // (ragged round its edge: a fleck of dust is not a drop)
                        let r = (r0 + (r1 - r0) * h(4) * h(4)) * (0.8 + 0.4 * value2((y - sy).atan2(x - sx) * 1.6 + 7.0 * h(5), 1.3, seed ^ 0x77));
                        let d = ((x - sx).powi(2) + (y - sy).powi(2)).sqrt();
                        c = c.max((1.0 - smooth(r, r + foot, d)) * (r / (r + foot)).min(1.0));
                    }
                }
            }
            strength * (k * c + (1.0 - k) * mean)
        };
        depth += specks(FLUO_GRAIN_CELL, (dirt * (0.35 + 0.75 * cloud)).min(1.0), 0.0001, 0.0005, FLUO_GRAIN, 60);
        depth += specks(FLUO_FLECK_CELL, 0.6 * dirt, 0.0004, 0.002, FLUO_FLECK, 70);
        depth
    }

    /// A tube's light along it at `s` from its middle, a share of its own:
    /// 1 over its body, dimmer over its electrode ends (`FLUO_ELECTRODE`
    /// diameters, `FLUO_ELECTRODE_LEVEL` at the cap), 0 past its glass.
    fn fluo_along(l: &FluoLayout, s: f32) -> f32 {
        let to_end = l.lit - s.abs();
        let d = 2.0 * l.radius;
        let end = smooth(0.0, FLUO_ELECTRODE * d, to_end);
        (FLUO_ELECTRODE_LEVEL + (1.0 - FLUO_ELECTRODE_LEVEL) * end) * smooth(-0.002, 0.002, to_end)
    }

    /// A tube's own brightness, a few percent either way (tubes age apart).
    fn fluo_gain(&self, i: i32) -> f32 {
        1.0 + 0.04 * (2.0 * hash01(i, 0, 0x7b3, self.seed) - 1.0)
    }

    /// The bare bank at `s` along and `t` across (short half sizes): the
    /// tubes as lit glass cylinders (a phosphor's light is even over the
    /// glass, a little brighter down its middle), their electrode ends, end
    /// caps and sockets, the clips that hold them; between and behind them
    /// the reflector, its folds as height (`plate` the polished trough's
    /// share, from crumpled foil at 0 to a trough per tube shaped to show
    /// its tube at 0.8; `foil_slope` for the foil), lit only by what its
    /// normal turns the viewer's ray onto: a tube's image (`FLUO_MIRROR` of
    /// its light, its ends and all) or the dark round the bank, over the
    /// little a dull reflector scatters (`FOIL_DULL`, `TROUGH_DULL`).
    fn fluorescent_bay(&self, l: &FluoLayout, s: f32, t: f32) -> f32 {
        let r = l.radius;
        let d = 2.0 * r;
        let i = (((t + l.bay) / l.pitch).floor() as i32).max(0).min(l.n as i32 - 1);
        let centre = |j: i32| -l.bay + (j as f32 + 0.5) * l.pitch;
        let off = t - centre(i);
        let x = off / r;
        // The tube in front: its glass, then its cap and its socket.
        let edge = (0.01 / r).min(0.1);
        let glass = 1.0 - smooth(1.0 - edge, 1.0 + edge, x.abs());
        let past = s.abs() - l.lit;
        let cap = if past > 0.0 && past < FLUO_CAP * d { 1.0 } else { 0.0 };
        let socket = if past >= FLUO_CAP * d && past < (FLUO_CAP + FLUO_SOCKET) * d { 1.0 - smooth(1.15 - edge, 1.15 + edge, x.abs()) } else { 0.0 };
        // The glass a little brighter down its middle and darker at its
        // edge, where the phosphor is seen edge on (the Kino Bank's dark
        // line along each tube).
        let limb = (0.9 + 0.1 * (1.0 - x * x).max(0.0)) * (1.0 - 0.45 * smooth(0.8, 1.0, x.abs()));
        let tube = self.fluo_gain(i) * Self::fluo_along(l, s) * limb + cap * FLUO_CAP_LEVEL * (0.5 + 0.5 * (1.0 - x * x).max(0.0));
        // The clips: a dark bracket over each tube's upper edge near its ends.
        let clip_s = (s.abs() - FLUO_CLIP_AT * l.lit).abs();
        let clip = (1.0 - smooth(0.2 * d, 0.2 * d + 0.003, clip_s)) * smooth(0.75 - edge, 0.75 + edge, x) * (1.0 - smooth(1.35 - edge, 1.35 + edge, x));
        let front = glass * (1.0 - smooth(FLUO_CAP * d - 0.002, FLUO_CAP * d, past));
        if front >= 1.0 && clip <= 0.0 {
            return tube;
        }
        // The reflector behind: its slope across (the trough's, aimed so
        // its middle shows the tube, and the foil's crumple) and along.
        let k = (self.plate / 0.8).min(1.0);
        let zt = FLUO_STANDOFF * r;
        let foil = 1.0 - k;
        let (ga, gb) = if foil > 0.0 {
            // Pulled taut along the bank, its wrinkles run along the tubes;
            // its crumple keeps its size in the foil, so it is laid out in
            // tube radii (`FOIL_RADIUS` of a short half size in the sizes
            // `foil_slope` is tuned at), not in the face's size.
            let scale = FOIL_RADIUS / r;
            // (its slopes, the crumple's angles, stay as they are).
            let g = foil_slope(vec2f(s * FOIL_ALONG, t) * scale, self.seed ^ 0x5f1);
            (g.x * FOIL_ALONG * foil, g.y * foil)
        } else {
            (0.0, 0.0)
        };
        // A trough is only so wide (`TROUGH_REACH` radii each side of its
        // tube); past it, as round a lone tube, the reflector is flat.
        let trough = (0.5 * off.atan2(zt)).tan() * k * (1.0 - smooth((TROUGH_REACH - 0.5) * r, TROUGH_REACH * r, off.abs()));
        let n = vec3f(-(gb + trough), -ga, 1.0).normalize();
        // The viewer's ray turned by the reflector (t, s, z), and the first
        // tube it meets: the cross-section's nearest pass within a radius.
        let ray = n * (2.0 * n.z) - vec3f(0.0, 0.0, 1.0);
        let across = (ray.x * ray.x + ray.z * ray.z).sqrt().max(1.0e-6);
        let (mut seen, mut cover, mut nearest) = (0.0f32, 0.0f32, f32::INFINITY);
        for j in (i - 2).max(0)..=(i + 2).min(l.n as i32 - 1) {
            let dx = centre(j) - t;
            let along_ray = (dx * ray.x + zt * ray.z) / across;
            if along_ray <= 0.0 || along_ray >= nearest {
                continue;
            }
            let miss = (dx * ray.z - zt * ray.x).abs() / across;
            // A rough reflector blurs the image: the foil's crumple a wide
            // cone, a polished trough's a narrow one.
            let blur = mix(FOIL_BLUR, TROUGH_BLUR, k) * along_ray;
            let hit = 1.0 - smooth(0.85 * r - blur, r + blur, miss);
            if hit > 0.0 {
                let q = miss / r;
                let at = s + ray.y / across * along_ray;
                seen = hit * self.fluo_gain(j) * Self::fluo_along(l, at) * (0.9 + 0.1 * (1.0 - q * q).max(0.0));
                cover = hit;
                nearest = along_ray;
            }
        }
        // The dull light: what the nearest tubes put on the reflector there.
        let mut dull = 0.0f32;
        for j in (i - 1).max(0)..=(i + 1).min(l.n as i32 - 1) {
            let v = vec3f(centre(j) - t, 0.0, zt);
            let dist = v.length();
            dull += (n.dot(v / dist)).max(0.0) * (r / dist) * Self::fluo_along(l, s).max(0.2 * smooth(-4.0 * d, 0.0, -past));
        }
        // A seam where two troughs meet, half way between tubes.
        let seam = 1.0 - 0.4 * k * line((off.abs() - 0.5 * l.pitch).abs() * 0.5, 0.004);
        // Where it misses the tubes the mirror shows the lit reflector and
        // frame round it (`FLUO_AROUND`), dimmer toward the bank's ends.
        let around = FLUO_AROUND * (1.0 - cover) * Self::fluo_along(l, s).max(0.3 * smooth(-4.0 * d, 0.0, -past));
        let reflector = (FLUO_MIRROR * (seen + around) + mix(FOIL_DULL, TROUGH_DULL, k) * dull) * seam;
        let behind = mix(reflector, FLUO_SOCKET_LEVEL, socket);
        let shown = mix(behind, tube, front);
        mix(shown, FLUO_CLIP_LEVEL, clip)
    }

    /// The bank behind a cloth (`baffle` sets its depth, up to
    /// `FLUO_CLOTH_DEPTH`): each tube's light on it, a line source's
    /// D / (D² + x²) across and its ends' fall along, and the reflector's
    /// image of it further back, through the cloth's folds from its tension
    /// map (`diffuser_shade`: tied to the frame all round, every
    /// `FLUO_CLOTH_TIES` of a short half size, each tie pulling a small fan).
    fn fluorescent_cloth(&self, l: &FluoLayout, u: Vec2f, s: f32, t: f32) -> f32 {
        let depth = (FLUO_CLOTH_DEPTH * self.baffle).max(l.radius);
        let k = (self.plate / 0.8).min(1.0);
        let image = mix(0.35, FLUO_MIRROR, k);
        let behind = depth + 2.0 * FLUO_STANDOFF * l.radius;
        let ends = |dd: f32| ((l.lit - s) / dd).atan() + ((l.lit + s) / dd).atan();
        let mut light = 0.0f32;
        for j in 0..l.n as i32 {
            let x = t - (-l.bay + (j as f32 + 0.5) * l.pitch);
            let g = self.fluo_gain(j);
            light += g * (depth / (depth * depth + x * x) * ends(depth) + image * behind / (behind * behind + x * x) * ends(behind));
        }
        let aspect = self.aspect();
        let p = vec2f(u.x * aspect.x, u.y * aspect.y);
        let mut ties = Vec::new();
        for (side, length) in [(0, aspect.x), (1, aspect.y)] {
            let n = (2.0 * length / FLUO_CLOTH_TIES).round().max(1.0) as i32;
            for k in 0..=n {
                let along = -length + 2.0 * length * k as f32 / n as f32;
                for sign in [-1.0f32, 1.0] {
                    ties.push(if side == 0 { vec2f(along, sign * aspect.y) } else { vec2f(sign * aspect.x, along) });
                }
            }
        }
        let shade = diffuser_shade(p, &ties, FLUO_CLOTH_FANS, aspect, &|_| f32::INFINITY, self.seed ^ 0x3c1);
        // In units of a tube's light: a bare tube's level spread over its pitch.
        light * l.radius * shade / std::f32::consts::PI
    }

    /// A fluorescent bank's mean over its box: a midpoint sum fine enough
    /// across to see every tube (16 samples a pitch at least).
    fn fluorescent_mean_tint(&self) -> (f32, Option<Vec3f>) {
        let l = self.fluo_layout();
        let (mut na, mut nc) = (48usize, ((16.0 * l.n) as usize).max(96).min(800));
        // A cover's cells, prisms and grime need finer steps: eight to a cell or a prism at least.
        match self.fluo_cover() {
            FluoCover::Open => {}
            FluoCover::Louvre(cells) => {
                nc = nc.max((8.0 * cells) as usize).min(1200);
                na = na.max((8.0 * cells * l.bay_long / l.bay) as usize).min(2400);
            }
            FluoCover::Prism(_) => {
                nc = nc.max((16.0 * l.bay / FLUO_PRISM_PITCH) as usize).min(1600);
                na = na.max(400);
            }
        }
        let mut sum = 0.0f64;
        let (mut lit, mut tinted, mut any) = ([0.0f64; 3], 0.0f64, false);
        for j in 0..nc {
            for i in 0..na {
                let (a, c) = (2.0 * (i as f32 + 0.5) / na as f32 - 1.0, 2.0 * (j as f32 + 0.5) / nc as f32 - 1.0);
                let u = if l.wide { vec2f(a, c) } else { vec2f(c, a) };
                let (f, t) = self.fluorescent_raw(u, 0.0);
                sum += f as f64;
                if let Some(t) = t {
                    any = true;
                    lit = [lit[0] + (f * t.x) as f64, lit[1] + (f * t.y) as f64, lit[2] + (f * t.z) as f64];
                    tinted += f as f64;
                } else {
                    lit = [lit[0] + f as f64, lit[1] + f as f64, lit[2] + f as f64];
                    tinted += f as f64;
                }
            }
        }
        let mean = ((sum / (na * nc) as f64) as f32).max(1.0e-3);
        let tint = (any && tinted > 0.0).then(|| vec3f((lit[0] / tinted) as f32, (lit[1] / tinted) as f32, (lit[2] / tinted) as f32));
        (mean, tint)
    }

    /// A street lamp as the night scenes show one: the luminaire, a disc of
    /// `plate` of the radius, 40 x its halo's first ring; the halo falling as
    /// 1 / (1 + (r / r0)²), half gone at r0 (`baffle` widens it, the night's
    /// haze), a tenth at 3 r0, a fiftieth at 7 r0; with `ribs` above 0 the
    /// pole under it, which hides the halo.
    fn street_lamp(&self, u: Vec2f, r: f32) -> f32 {
        let core_r = self.plate.max(0.02);
        let r0 = 0.08 + 0.25 * self.baffle;
        let halo = 1.0 / (1.0 + (r / r0).powi(2));
        let core = 40.0 * (1.0 - smooth(core_r - 0.01, core_r + 0.01, r)) * (0.8 + 0.2 * smooth(0.0, core_r, r));
        let mut pole = 1.0;
        if self.ribs >= 1.0 && u.y < -0.6 * core_r {
            let half_width = 0.6 * core_r.min(0.06);
            pole = 1.0 - 0.9 * (1.0 - smooth(half_width, half_width + EDGE, u.x.abs()));
        }
        (core + halo) * pole
    }
}

/// Hammered metal (a beauty dish's bowl, a reflector's): small flat facets
/// of one hashed point per cell, `across` cells over the box's width, each
/// of log-uniform brightness (`log_uniform` at `contrast`, mean 1: the
/// flash mirrored by facets at every tilt). No lines where two meet: the
/// plates show dimples, not a mosaic.
fn hammered(x: f32, y: f32, across: f32, contrast: f32, seed: u32) -> f32 {
    let (_, _, tilt) = cells(x * 0.5 * across + 3.1, y * 0.5 * across + 7.7, seed);
    log_uniform(tilt, contrast)
}

/// Crumpled foil's slope (its height's gradient) at `p` (short half sizes),
/// by the fabric recipe's layers with no pull points (a sheet pressed flat
/// on a board, held at its edges): the fine creases of the crumple, one to
/// a cell, each a crisp bend flattening out within a third of a cell; flat
/// facets, a Voronoi of tilted planes with crisp edges, as pressed foil's
/// are; and a fine crinkle in patches. Taken as the height's exact slope,
/// not a difference across a crease, so a crease is one clean line. Its
/// light comes only from the normal (`Modifier::fluorescent_bay`).
fn foil_slope(p: Vec2f, seed: u32) -> Vec2f {
    let mut g = vec2f(0.0, 0.0);
    let c = FOIL_CELL;
    let (gx, gy) = (p.x / c, p.y / c);
    let (fx, fy) = (gx.floor() as i32, gy.floor() as i32);
    for j in -1..=1i32 {
        for i in -1..=1i32 {
            let (cx, cy) = (fx + i, fy + j);
            let k = |n: i32| hash01(cx, cy, n, seed.wrapping_add(41));
            let (sa, ca) = (std::f32::consts::PI * k(2)).sin_cos();
            let (dx, dy) = (gx - cx as f32 - k(0), gy - cy as f32 - k(1));
            let along = dx * ca + dy * sa;
            let half_length = 0.6 + 0.6 * k(3);
            let window = 1.0 - smooth(0.4 * half_length, half_length, along.abs());
            if window <= 0.0 {
                continue;
            }
            let across = (dy * ca - dx * sa) * c;
            let bend = FOIL_BEND * (0.3 + 0.9 * k(4)) * if k(5) < 0.5 { -1.0 } else { 1.0 };
            let (reach, soft) = (0.35 * c, 0.15 * c);
            // d/d(across) of bend (sqrt(a² + s²) - s) e^(-(a / reach)²), the
            // flattening's own slope left out (it only rounds the bend's shoulders).
            let slope = bend * across / (across * across + soft * soft).sqrt() * (-(across / reach).powi(2)).exp() * window;
            g += vec2f(-sa, ca) * slope;
        }
    }
    // The facets: the nearest cell's plane of a jittered lattice.
    let f = FOIL_FACETS;
    let (x, y) = (p.x * f + 0.37, p.y * f + 2.9);
    let (ix, iy) = (x.floor(), y.floor());
    let (mut nearest, mut plane) = (f32::INFINITY, vec2f(0.0, 0.0));
    for j in -1..=1i32 {
        for i in -1..=1i32 {
            let (cx, cy) = (ix as i32 + i, iy as i32 + j);
            let dx = x - (ix + i as f32 + hash01(cx, cy, 0, seed ^ 0x3f1));
            let dy = y - (iy + j as f32 + hash01(cx, cy, 1, seed ^ 0x3f1));
            let d = dx * dx + dy * dy;
            if d < nearest {
                nearest = d;
                plane = vec2f(2.0 * hash01(cx, cy, 2, seed ^ 0x3f1) - 1.0, 2.0 * hash01(cx, cy, 3, seed ^ 0x3f1) - 1.0);
            }
        }
    }
    g += plane * FOIL_TILT;
    // The crinkle, in patches: a fine noise's slope.
    let patch = patches(p.x, p.y, 1.3, 0.4, 0.7, 2, seed.wrapping_add(7));
    if patch > 0.0 {
        let e = 0.25 / FOIL_CRINKLE;
        let at = |q: Vec2f| fbm2(q.x * FOIL_CRINKLE + 5.1, q.y * FOIL_CRINKLE + 2.3, 2, seed.wrapping_add(9));
        let h0 = at(p);
        g += vec2f(at(p + vec2f(e, 0.0)) - h0, at(p + vec2f(0.0, e)) - h0) * (FOIL_CRINKLE_AMP * patch / e);
    }
    g
}

/// The side of a white beauty dish's sock the flash lights more, as the
/// angle from straight up, clockwise (RLS002: its lower right).
const SOCK_LIT_SIDE: f32 = 2.7;
/// The sock's packing creases (folded in half and in half again: one
/// along a diameter, one along a radius across it), their height and
/// width in radii.
const SOCK_CREASE_AMP: f32 = 0.0018;
const SOCK_CREASE_WIDTH: f32 = 0.014;
/// The crumple of a sock packed away and pulled taut again: short crisp
/// creases where the cloth was folded flat, most along its weave (up and
/// down or across, give or take a quarter of a right angle), one in
/// `SOCK_CRUMPLE_SHARE` cells of a grid `SOCK_CRUMPLE_CELL` of the radius,
/// each a ridge 1.2 to 3.2 cells long, a little bent, and about
/// `SOCK_CREASE_WIDTH` wide.
const SOCK_CRUMPLE_AMP: f32 = 0.0013;
const SOCK_CRUMPLE_CELL: f32 = 0.09;
const SOCK_CRUMPLE_SHARE: f32 = 0.6;
/// The slack middle's broad soft folds.
const SOCK_BROAD_AMP: f32 = 0.012;
const SOCK_BROAD_FREQ: f32 = 1.6;

/// A beauty dish sock's height at `u` (radii), by the fabric recipe. Its
/// elastic hem pulls it evenly all round the lip, so no fans run in from
/// pull points (RLS002 shows none): broad soft folds where the middle is
/// slack; the crumple of its packing, short creases mostly along its
/// weave; and the two straight packing creases (RLS002's cross), each a
/// sharp ridge that wanders and breaks into dashes.
fn sock_height(u: Vec2f, seed: u32) -> f32 {
    let r = u.length();
    // The slack middle: broad soft folds.
    let (bx, by) = (u.x * SOCK_BROAD_FREQ, u.y * SOCK_BROAD_FREQ);
    let warp = fbm2(bx * 0.6 + 4.1, by * 0.6 + 8.2, 2, seed.wrapping_add(19)) - 0.5;
    let mut h = SOCK_BROAD_AMP * (1.0 - r * r).max(0.0) * (fbm2(bx + warp, by - warp, 3, seed.wrapping_add(23)) - 0.5);
    // The crumple: short creases, a few to a cell's neighbourhood.
    let (gx, gy) = (u.x / SOCK_CRUMPLE_CELL, u.y / SOCK_CRUMPLE_CELL);
    let (ix, iy) = (gx.floor() as i32, gy.floor() as i32);
    for j in -2..=2i32 {
        for i in -2..=2i32 {
            let (cx, cy) = (ix + i, iy + j);
            let k = |n: i32| hash01(cx, cy, n, seed.wrapping_add(61));
            if k(0) >= SOCK_CRUMPLE_SHARE {
                continue;
            }
            let centre = vec2f(cx as f32 + k(1), cy as f32 + k(2));
            let angle = if k(3) < 0.5 { 0.0 } else { std::f32::consts::FRAC_PI_2 } + 0.8 * (k(4) - 0.5);
            let (sa, ca) = angle.sin_cos();
            let (dx, dy) = (gx - centre.x, gy - centre.y);
            let along = (dx * ca + dy * sa) / (0.6 + k(5));
            let across = (dy * ca - dx * sa + 0.25 * (k(8) - 0.5) * along * along) * SOCK_CRUMPLE_CELL;
            let w = SOCK_CREASE_WIDTH * (0.6 + 0.6 * k(6));
            h += SOCK_CRUMPLE_AMP * (0.5 + k(7)) * (1.0 - smooth(0.6, 1.0, along.abs())) * (-(across * across + 0.03 * w * w).sqrt() / w).exp();
        }
    }
    // The packing creases: a diameter (straight up and down) and a radius
    // from the middle to the right, each a little off the middle.
    let x0 = 0.06 * (hash01(1, 0, 7, seed) - 0.5) - 0.02;
    let y0 = 0.08 * (hash01(2, 0, 7, seed) - 0.5) - 0.1;
    let packed = |d: f32, t: f32, s: u32| {
        let d = d + 0.012 * (2.0 * fbm2(t * 3.0 + 1.7, 0.5, 2, seed.wrapping_add(s)) - 1.0) + 0.005 * (2.0 * fbm2(t * 9.0 + 3.9, 7.5, 2, seed.wrapping_add(s + 2)) - 1.0);
        let strength = 0.4 + 0.8 * smooth(0.35, 0.65, fbm2(t * 3.0 + 6.3, 2.5, 2, seed.wrapping_add(s + 1)));
        let w = SOCK_CREASE_WIDTH;
        strength * (-(d * d + 0.03 * w * w).sqrt() / w).exp()
    };
    h += 1.6 * SOCK_CREASE_AMP * packed(u.x - x0, u.y, 31);
    h += 0.7 * SOCK_CREASE_AMP * packed(u.y - y0, u.x, 37) * smooth(x0 - 0.02, x0 + 0.04, u.x);
    h
}

/// A beauty dish sock's cloth as the bowl behind it lights it, a factor
/// round 1, through the height's normal: the white bowl fills the half of
/// the sky behind the cloth, so a facet tilted by its normal takes
/// (1 + n.z) / 2 of the flat cloth's light, and its light crosses more
/// cloth (n.z squared), so a crease darkens on both sides, as RLS002's do.
fn sock_shade(u: Vec2f, seed: u32) -> f32 {
    let e = 0.004;
    let h0 = sock_height(u, seed);
    let hx = (sock_height(u + vec2f(e, 0.0), seed) - h0) / e;
    let hy = (sock_height(u + vec2f(0.0, e), seed) - h0) / e;
    let nz = 1.0 / (1.0 + hx * hx + hy * hy).sqrt();
    0.5 * (1.0 + nz) * nz * nz
}

/// The panel's names for `MODIFIER_KIND_NAMES`, in its order.
pub const MODIFIER_KIND_LABELS: &[&str] = &[
    "Plain", "Softbox", "Strip box", "Octabox", "Umbrella", "Silver umbrella", "Beauty dish", "Ring light", "Fresnel spot", "Bare bulb", "LED panel", "Gradient", "Street lamp", "Window", "Reflector", "Fluorescent", "Candle",
];

/// Makes `light` a light of `kind` as a studio has it, where it is: its
/// shape, its size as seen from a few metres off, its edge, its colour and
/// its modifier. Its place, roll, name, brightness and key are kept. "Plain"
/// takes the modifier (and a window's glass) off and keeps the rest.
pub fn apply_template(light: &mut super::params::LightParams, kind: ModifierKind) {
    use super::params::{GoboParams, PolygonParams};
    let modifier = |kind: &str, rim: f32, baffle: f32, ribs: u32, plate: f32| Some(ModifierParams { kind: kind.to_string(), rim, baffle, ribs, plate, diffusion: None, across: None, ..ModifierParams::default() });
    let window_glass = light.gobo.as_ref().is_some_and(|g| g.fill_sky);
    if window_glass && kind != ModifierKind::Window {
        light.gobo = None;
    }
    let mut set = |shape: &str, w: f32, h: f32, corner: f32, softness: f32, hotspot: f32, kelvin: f32| {
        light.shape = shape.to_string();
        light.width_deg = w;
        light.height_deg = h;
        light.corner = corner;
        light.softness = softness;
        light.hotspot = hotspot;
        light.kelvin = kelvin;
        light.rgb = None;
        light.source = None;
    };
    match kind {
        ModifierKind::None => {
            light.modifier = None;
            return;
        }
        ModifierKind::Softbox => set("rect", 24.0, 18.0, 0.02, 0.02, 0.0, 5600.0),
        ModifierKind::Strip => set("rect", 8.0, 36.0, 0.02, 0.02, 0.0, 5600.0),
        ModifierKind::Octabox => set("polygon", 28.0, 28.0, 0.0, 0.02, 0.0, 5600.0),
        ModifierKind::Umbrella => set("disc", 32.0, 32.0, 0.0, 0.02, 0.0, 5600.0),
        ModifierKind::SilverUmbrella => set("disc", 30.0, 30.0, 0.0, 0.02, 0.0, 5600.0),
        ModifierKind::BeautyDish => set("disc", 16.0, 16.0, 0.0, 0.02, 0.0, 5600.0),
        ModifierKind::RingLight => set("ring", 18.0, 18.0, 0.0, 0.03, 0.0, 5600.0),
        ModifierKind::Fresnel => set("disc", 7.0, 7.0, 0.0, 0.03, 0.0, 3200.0),
        ModifierKind::Bulb => set("disc", 3.0, 3.0, 0.0, 0.1, 0.0, 2700.0),
        ModifierKind::LedPanel => set("rect", 16.0, 10.0, 0.03, 0.02, 0.0, 5600.0),
        ModifierKind::Gradient => set("rect", 20.0, 20.0, 0.0, 0.02, 0.0, 5600.0),
        ModifierKind::StreetLamp => set("disc", 8.0, 8.0, 0.0, 0.5, 0.0, 6000.0),
        ModifierKind::Window => set("rect", 40.0, 50.0, 0.0, 0.01, 0.0, 6500.0),
        // A 100 x 140 cm panel a couple of metres off; its outline is its own (reflector.rs).
        ModifierKind::Reflector => set("rect", 26.0, 37.0, 0.0, 0.01, 0.0, 5600.0),
        // A four-tube bank (125 x 33 cm) a couple of metres off.
        ModifierKind::Fluorescent => set("rect", 28.0, 8.0, 0.0, 0.01, 0.0, 5600.0),
        // A tea light in a votive on a table (clamp() stands it in 3D as its flame, `hold_candle`).
        ModifierKind::Candle => {
            set("disc", 1.0, 1.0, 0.0, 0.0, 0.0, super::params::CANDLE_KELVIN);
            light.intensity_ev = super::params::CANDLE_INTENSITY_EV;
            light.key = false;
        }
    }
    // Plan P1: a light placed in 3D takes the template's size where it
    // stands (its angular size at its distance) and the template's beam
    // (`space::template_beam`); a bare bulb glows all round, its face turned
    // back to the camera.
    let (width_deg, height_deg) = (light.width_deg, light.height_deg);
    if let Some(space) = light.space.as_mut() {
        let d = space.distance().max(super::params::SPACE_NEAR_M);
        let size = |deg: f32| 2.0 * d * (0.5 * deg).to_radians().tan();
        space.width_m = size(width_deg);
        space.height_m = size(height_deg);
        space.beam = super::space::template_beam(kind);
        if kind == ModifierKind::Bulb {
            space.yaw_deg = 0.0;
            space.pitch_deg = 0.0;
        }
    }
    light.modifier = match kind {
        ModifierKind::Softbox => modifier("softbox", 0.025, 0.0, 0, 0.0),
        ModifierKind::Strip => modifier("strip", 0.03, 0.0, 0, 0.0),
        ModifierKind::Octabox => modifier("octabox", 0.02, 0.0, 8, 0.0),
        ModifierKind::Umbrella => modifier("umbrella", 0.03, 0.6, 8, 0.3),
        ModifierKind::SilverUmbrella => modifier("silver_umbrella", 0.02, 0.8, 16, 0.2),
        ModifierKind::BeautyDish => modifier("beauty_dish", 0.03, 0.0, 0, 0.32),
        ModifierKind::RingLight => modifier("ring_light", 0.05, 0.0, 48, 0.55),
        ModifierKind::Fresnel => modifier("fresnel", 0.12, 1.0, 14, 0.0),
        ModifierKind::Bulb => modifier("bulb", 0.0, 0.0, 0, 0.45),
        ModifierKind::LedPanel => modifier("led_panel", 0.02, 0.0, 20, 0.0),
        ModifierKind::Gradient => modifier("gradient", 0.0, 0.15, 0, 0.0),
        ModifierKind::StreetLamp => modifier("street_lamp", 0.0, 0.3, 1, 0.1),
        ModifierKind::Window => modifier("window", 0.0, 0.0, 0, 0.0),
        ModifierKind::Reflector => modifier("reflector", REFLECTOR_BINDING, 0.5, 0, 0.5),
        ModifierKind::Fluorescent => modifier("fluorescent", 0.04, 0.0, 4, 0.8),
        ModifierKind::Candle => Some(ModifierParams { kind: "candle".to_string(), rim: 0.0, baffle: 0.0, ribs: 0, plate: 0.0, candle: Some(super::params::CandleParams::default()), ..ModifierParams::default() }),
        ModifierKind::None => None,
    };
    match kind {
        ModifierKind::Octabox => light.polygon = Some(PolygonParams { sides: 8, ..PolygonParams::default() }),
        ModifierKind::RingLight => light.inner = 0.55,
        // A window: glass the map's sky shows through, in a frame of four by three panes.
        ModifierKind::Window => {
            light.gobo = Some(GoboParams { kind: "window".to_string(), panes_x: 3, panes_y: 4, fill_sky: true, ..GoboParams::default() });
            light.intensity_ev = 0.0;
        }
        _ => {}
    }
}

/// The template a light shows in the panel's Type list: its modifier's kind, or a window by its glass.
pub fn kind_of(light: &super::params::LightParams) -> ModifierKind {
    match light.modifier.as_ref().map(ModifierParams::kind) {
        Some(kind) => kind,
        None if light.gobo.as_ref().is_some_and(|g| g.fill_sky) => ModifierKind::Window,
        None => ModifierKind::None,
    }
}


#[cfg(test)]
mod led_tests {
    use super::*;

    fn led(kind: &str, rim: f32, baffle: f32, ribs: u32, plate: f32) -> Modifier {
        let p = ModifierParams { kind: kind.to_string(), rim, baffle, ribs, plate, diffusion: None, across: None, ..ModifierParams::default() };
        Modifier::new(&p, vec2f(0.27, 0.27)).unwrap()
    }

    /// The face's mean over the unit disc's ring from `hole` (the box when
    /// `round` is false), a 1200 x 1200 midpoint sum.
    fn mean(m: &Modifier, round: bool, hole: f32) -> f32 {
        let n = 1200;
        let (mut sum, mut count) = (0.0f64, 0u32);
        for j in 0..n {
            for i in 0..n {
                let u = vec2f(2.0 * (i as f32 + 0.5) / n as f32 - 1.0, 2.0 * (j as f32 + 0.5) / n as f32 - 1.0);
                let r = u.length();
                if round && (r > 1.0 || r < hole) {
                    continue;
                }
                sum += m.factor(vec2f(u.x * 0.27, u.y * 0.27)) as f64;
                count += 1;
            }
        }
        (sum / count as f64) as f32
    }

    #[test]
    fn led_panels_bare_diffused_and_bicolour_sit_at_mean_one() {
        for (baffle, plate) in [(0.0, 0.0), (0.0, 0.8), (0.25, 0.0), (0.5, 0.0), (0.5, 0.8), (0.75, 0.4)] {
            let m = mean(&led("led_panel", 0.02, baffle, 20, plate), false, 0.0);
            assert!((m - 1.0).abs() < 0.06, "baffle {baffle} plate {plate}: mean {m}");
        }
    }

    #[test]
    fn a_nude_led_sits_at_mean_one_its_chip_the_most_of_it() {
        let m = led("led_panel", 0.045, 0.0, 0, 0.22);
        let all = mean(&m, true, 0.0);
        assert!((all - 1.0).abs() < 0.03, "mean {all}");
        // RLS037: the chip about 15 x the face's mean, the well round it under a fiftieth, the reflector a fifth.
        let at = |r: f32| m.factor(vec2f(0.0, r * 0.27));
        assert!(at(0.15) > 8.0 && at(0.15) < 25.0, "chip {}", at(0.15));
        assert!(at(0.5) < 0.03, "well {}", at(0.5));
        let rim: f32 = (0..64).map(|i| m.factor(vec2f(0.27 * 0.88 * (i as f32 * 0.1).sin(), 0.27 * 0.88 * (i as f32 * 0.1).cos()))).sum::<f32>() / 64.0;
        assert!(rim > 0.08 && rim < 0.4, "reflector {rim}");
    }

    #[test]
    fn ring_lights_bare_and_diffused_sit_at_mean_one_over_the_ring() {
        for (rim, plate, baffle, ribs) in [(0.01, 0.51, 0.0, 48), (0.01, 0.51, 1.0, 48), (0.05, 0.55, 0.0, 48), (0.05, 0.75, 0.5, 24)] {
            let m = mean(&led("ring_light", rim, baffle, ribs, plate), true, plate);
            assert!((m - 1.0).abs() < 0.04, "rim {rim} plate {plate} baffle {baffle}: mean {m}");
        }
    }

    #[test]
    fn a_thick_diffuser_softens_a_rings_diodes_into_glows() {
        // LED_Ring (the area map): one row of soft glows on evenly lit plastic, about 2-3 x the gaps, no dark lips.
        let ring = |diffusion: Option<f32>| {
            let p = ModifierParams { kind: "ring_light".to_string(), rim: 0.01, baffle: 1.0, ribs: 8, plate: 0.755, diffusion, ..ModifierParams::default() };
            Modifier::new(&p, vec2f(0.27, 0.27)).unwrap()
        };
        let (bare, thick) = (ring(None), ring(Some(0.2)));
        let m = mean(&thick, true, 0.755);
        assert!((m - 1.0).abs() < 0.04, "mean {m}");
        // Round the row: the brightest and the darkest point of the ring at its middle.
        let around = |m: &Modifier| {
            let r = 0.5 * (0.755 + 0.99);
            let v: Vec<f32> = (0..2000).map(|i| {
                let a = std::f32::consts::TAU * i as f32 / 2000.0;
                m.factor(vec2f(0.27 * r * a.sin(), 0.27 * r * a.cos()))
            }).collect();
            (v.iter().cloned().fold(0.0f32, f32::max), v.iter().cloned().fold(f32::INFINITY, f32::min))
        };
        let (hi, lo) = around(&thick);
        assert!(hi > 1.6 * lo && hi < 4.0 * lo, "glows {hi} over gaps {lo}");
        let (bhi, blo) = around(&bare);
        assert!(bhi / blo > hi / lo, "the bare dots are crisper");
        // No dark lips: just inside the outer edge the plastic is still lit, at least a third of the gaps.
        let lip = thick.factor(vec2f(0.0, 0.27 * 0.975));
        assert!(lip > 0.33 * lo, "lip {lip} gaps {lo}");
    }

    /// A face's mean over its box on an `nx` x `ny` midpoint grid.
    fn box_mean(m: &Modifier, half: Vec2f, nx: usize, ny: usize) -> f32 {
        let mut sum = 0.0f64;
        for j in 0..ny {
            for i in 0..nx {
                let q = vec2f(half.x * (2.0 * (i as f32 + 0.5) / nx as f32 - 1.0), half.y * (2.0 * (j as f32 + 0.5) / ny as f32 - 1.0));
                sum += m.factor(q) as f64;
            }
        }
        (sum / (nx * ny) as f64) as f32
    }

    #[test]
    fn fluorescent_banks_bare_troughed_and_clothed_sit_at_mean_one() {
        for (half, ribs, plate, baffle) in [
            (vec2f(0.36, 0.08), 4, 0.8, 0.0),
            (vec2f(0.36, 0.08), 4, 0.0, 0.0),
            (vec2f(0.27, 0.27), 16, 0.0, 0.0),
            (vec2f(0.27, 0.26), 16, 0.0, 0.6),
            (vec2f(0.08, 0.36), 6, 0.4, 0.0),
            (vec2f(0.45, 0.035), 1, 0.8, 0.0),
        ] {
            let p = ModifierParams { kind: "fluorescent".to_string(), rim: 0.03, baffle, ribs, plate, diffusion: None, across: None, ..ModifierParams::default() };
            let m = Modifier::new(&p, half).unwrap();
            let (nx, ny) = if half.x >= half.y { (400, 1600) } else { (1600, 400) };
            let mean = box_mean(&m, half, nx, ny);
            assert!((mean - 1.0).abs() < 0.05, "half {half:?} ribs {ribs} plate {plate} baffle {baffle}: mean {mean}");
        }
    }

    fn fluo_cover(half: Vec2f, ribs: u32, baffle: f32, diffusion: Option<f32>) -> Modifier {
        let p = ModifierParams { kind: "fluorescent".to_string(), rim: 0.02, baffle, ribs, plate: 0.8, diffusion, across: None, ..ModifierParams::default() };
        Modifier::new(&p, half).unwrap()
    }

    /// A cover leaves the bank at mean 1: an egg-crate over bare tubes and over the cloth, a troffer's louvre, a
    /// clean and a dirty prismatic cover.
    #[test]
    fn covered_fluorescent_banks_sit_at_mean_one() {
        for (half, ribs, baffle, diffusion) in [
            (vec2f(0.36, 0.164), 4, 0.0, 0.4),
            (vec2f(0.36, 0.164), 4, 1.0, 0.4),
            (vec2f(0.27, 0.27), 8, 0.0, 0.04),
            (vec2f(0.36, 0.087), 2, 0.0, 0.52),
            (vec2f(0.36, 0.087), 2, 0.0, 0.75),
            (vec2f(0.087, 0.36), 2, 0.0, 0.75),
        ] {
            let m = fluo_cover(half, ribs, baffle, Some(diffusion));
            let (nx, ny) = if half.x >= half.y { (2400, 1200) } else { (1200, 2400) };
            let mean = box_mean(&m, half, nx, ny);
            assert!((mean - 1.0).abs() < 0.05, "half {half:?} ribs {ribs} baffle {baffle} diffusion {diffusion}: mean {mean}");
        }
    }

    /// No cover (`diffusion` none or 0) is the open bank, point for point; an egg-crate's front edges are dark over
    /// the middle, where its cells show the tubes as the open bank does; toward the face's ends, seen at a slant,
    /// its matte walls take a share of every cell.
    #[test]
    fn an_egg_crate_shows_its_edges_and_toward_the_ends_its_walls() {
        let half = vec2f(0.36, 0.164);
        let open = fluo_cover(half, 4, 0.0, None);
        let zero = fluo_cover(half, 4, 0.0, Some(0.0));
        for i in 0..50 {
            let q = vec2f(0.35 * (i as f32 * 0.37).sin(), 0.16 * (i as f32 * 0.91).cos());
            assert_eq!(open.factor(q), zero.factor(q), "{q:?}");
        }
        let crate_ = fluo_cover(half, 4, 0.0, Some(0.4));
        // Forty cells over the bay (0.98 of the short half each way): a wall's front edge, a cell's middle.
        let pitch = 2.0 * 0.98 / 40.0 * half.y;
        let edge = crate_.factor(vec2f(0.0, -0.98 * half.y + 20.0 * pitch));
        let cell = |x: f32| (0..40).map(|k| crate_.factor(vec2f(x, -0.98 * half.y + (k as f32 + 0.5) * pitch))).sum::<f32>() / 40.0;
        assert!(edge < 0.1 * cell(0.0), "edge {edge} cell {}", cell(0.0));
        // Along a row near an end the walls cut into the cells: its mean falls below the open bank's there.
        let row = |m: &Modifier, x0: f32| (0..400).map(|k| m.factor(vec2f(x0 + 0.04 * k as f32 / 400.0, 0.3 * half.y))).sum::<f32>();
        let ratio = |x0: f32| row(&crate_, x0) * crate_.norm / (row(&open, x0) * open.norm);
        assert!(ratio(-0.3) < 0.9 * ratio(-0.02), "at the end {} in the middle {}", ratio(-0.3), ratio(-0.02));
    }

    /// A dirty cover's dust only takes light: over the same bank its face is a little dimmer on average than a clean
    /// cover's (before both sit at mean 1), never brighter at any point, and under a fleck far darker.
    #[test]
    fn a_dirty_covers_dust_takes_light() {
        let half = vec2f(0.36, 0.087);
        let clean = fluo_cover(half, 2, 0.0, Some(0.52));
        let dirty = fluo_cover(half, 2, 0.0, Some(0.8));
        let row = |m: &Modifier, y: f32| (0..4000).map(|k| m.factor(vec2f(half.x * (1.9 * k as f32 / 4000.0 - 0.95), y)) * m.norm).collect::<Vec<f32>>();
        for y in [-0.5 * half.y, 0.0, 0.4 * half.y] {
            let (c, d) = (row(&clean, y), row(&dirty, y));
            let (mc, md) = (c.iter().sum::<f32>() / 4000.0, d.iter().sum::<f32>() / 4000.0);
            assert!(md < 0.95 * mc && md > 0.4 * mc, "mean {md} clean {mc} at {y}");
            // point by point over the same bank: never brighter, and somewhere a fleck takes most of it
            let ratio: Vec<f32> = c.iter().zip(&d).filter(|(c, _)| **c > 1.0e-3).map(|(c, d)| d / c).collect();
            let (lo, hi) = (ratio.iter().cloned().fold(f32::INFINITY, f32::min), ratio.iter().cloned().fold(0.0f32, f32::max));
            assert!(hi <= 1.0001 && lo < 0.5, "dirty over clean {lo}..{hi} at {y}");
        }
    }

    /// A dirty cover's dust settles against its frame: along each of the four edges it lies deeper than down the
    /// cover's middle.
    #[test]
    fn a_dirty_covers_dust_gathers_along_every_edge() {
        let half = vec2f(0.36, 0.087);
        let m = fluo_cover(half, 2, 0.0, Some(0.8));
        let l = m.fluo_layout();
        let a = m.aspect();
        let (ex, ey) = (l.bay_long, l.bay);
        let depth = |p: Vec2f| m.fluo_dirt_depth(&l, vec2f(p.x / a.x, p.y / a.y), 0.8, 0.0);
        let mean = |f: &dyn Fn(f32) -> Vec2f| (0..4000).map(|k| depth(f(k as f32 / 4000.0)) as f64).sum::<f64>() / 4000.0;
        let middle = mean(&|t| vec2f(ex * (t - 0.5), 0.0));
        let edges = [
            ("bottom", mean(&|t| vec2f(ex * (1.6 * t - 0.8), -0.85 * ey))),
            ("top", mean(&|t| vec2f(ex * (1.6 * t - 0.8), 0.85 * ey))),
            ("left", mean(&|t| vec2f(-ex + 0.15 * ey, ey * (1.6 * t - 0.8)))),
            ("right", mean(&|t| vec2f(ex - 0.15 * ey, ey * (1.6 * t - 0.8)))),
        ];
        for (name, d) in edges {
            assert!(d > 1.3 * middle, "{name} {d} middle {middle}");
        }
    }

    /// A dirty cover's dust is grey-brown (it takes blue most, red least) at the level `factor` gives it, and only
    /// a dirty prismatic cover has it: a clean cover and every other face keep the light's own colour.
    #[test]
    fn a_dirty_covers_dust_is_brownish() {
        let half = vec2f(0.36, 0.087);
        let dirty = fluo_cover(half, 2, 0.0, Some(0.8));
        let darkest = (0..4000)
            .map(|k| vec2f(half.x * 0.9 * (2.0 * k as f32 / 4000.0 - 1.0), -0.5 * half.y))
            .min_by(|a, b| dirty.factor(*a).total_cmp(&dirty.factor(*b)))
            .unwrap();
        let t = dirty.tint(darkest).expect("a stain is tinted");
        assert!(t.x > t.y && t.y > t.z, "{t:?}");
        assert!((luminance(t) - 1.0).abs() < 1.0e-4, "{t:?}");
        assert_eq!(fluo_cover(half, 2, 0.0, Some(0.52)).tint(darkest), None, "a clean cover");
        assert_eq!(fluo_cover(half, 2, 0.0, Some(0.3)).tint(darkest), None, "a louvre");
        let softbox = Modifier::new(&ModifierParams { kind: "softbox".to_string(), ..ModifierParams::default() }, vec2f(0.3, 0.3)).unwrap();
        assert_eq!(softbox.tint(vec2f(0.0, 0.0)), None, "a softbox");
    }

    /// A single-chip head (`diffusion` above 0 on a nude LED) sits at mean 1, its dies brighter than the phosphor
    /// between them, a screw ear's head dark on the holder's ring, its reflector near black.
    #[test]
    fn a_single_chip_head_sits_at_mean_one_its_dies_apart() {
        let p = ModifierParams { kind: "led_panel".to_string(), rim: 0.01, baffle: 0.0, ribs: 0, plate: 0.215, diffusion: Some(0.8), across: None, ..ModifierParams::default() };
        let m = Modifier::new(&p, vec2f(0.27, 0.27)).unwrap();
        let all = mean(&m, true, 0.0);
        assert!((all - 1.0).abs() < 0.04, "mean {all}");
        let chip: Vec<f32> = (0..400).map(|k| m.factor(vec2f(0.27 * 0.12 * (k as f32 * 0.137).sin(), 0.27 * 0.12 * (k as f32 * 0.071).cos()))).collect();
        let (lo, hi) = chip.iter().fold((f32::INFINITY, 0.0f32), |(a, b), v| (a.min(*v), b.max(*v)));
        assert!(hi > 2.5 * lo, "dies {hi} gaps {lo}");
        let at = |a: f32, r: f32| m.factor(vec2f(0.27 * r * a.cos(), 0.27 * r * a.sin()));
        let (ear, ring) = (at(SINGLE_EAR_ANGLE, SINGLE_EAR_AT * 0.215), at(SINGLE_EAR_ANGLE + 1.0, 1.63 * 0.215));
        assert!(ear < 0.5 * ring, "screw {ear} holder ring {ring}");
        assert!(at(0.3, 0.7) < 0.004, "reflector {}", at(0.3, 0.7));
    }

    #[test]
    fn a_fluorescent_tube_is_brighter_than_the_reflector_between() {
        let half = vec2f(0.36, 0.08);
        let p = ModifierParams { kind: "fluorescent".to_string(), rim: 0.03, baffle: 0.0, ribs: 4, plate: 0.0, diffusion: None, across: None, ..ModifierParams::default() };
        let m = Modifier::new(&p, half).unwrap();
        // The bay is 0.97 of the short half across, four tubes: the first tube's
        // middle and the gap past it, in short half sizes.
        let pitch = 2.0 * 0.97 / 4.0;
        let at = |t: f32| m.factor(vec2f(0.0, (-0.97 + t * pitch) * half.y));
        assert!(at(0.5) > 2.0 * at(1.0), "tube {} gap {}", at(0.5), at(1.0));
    }

    #[test]
    fn a_white_beauty_dish_sits_at_mean_one_bright_round_its_plate() {
        let p = ModifierParams { kind: "beauty_dish".to_string(), rim: 0.02, baffle: 0.0, ribs: 0, plate: 0.28, diffusion: Some(0.8), across: None, ..ModifierParams::default() };
        let m = Modifier::new(&p, vec2f(0.27, 0.27)).unwrap();
        let mean = mean(&m, true, 0.0);
        assert!((mean - 1.0).abs() < 0.03, "mean {mean}");
        // HDR Light Studio's White Bare: about 5 x just past the plate's rim, under 1 at 0.75.
        // Down and to the right, between two rods.
        let at = |r: f32| m.factor(vec2f(0.27 * r * 0.7, -0.27 * r * 0.714));
        assert!(at(0.32) > 3.0 * at(0.75), "ring {} bowl {}", at(0.32), at(0.75));
    }

    /// The area map's LED_Ring (one row of diodes in a wide ring, `RING_PITCH`)
    /// and LED_Panel (a thick diffuser's tiles, `led_tiles`) at mean 1, the
    /// ring's 36 diodes round it.
    #[test]
    fn a_one_row_ring_and_a_tiled_panel_sit_at_mean_one() {
        let ring = led("ring_light", 0.01, 1.0, 8, 0.755);
        let m = mean(&ring, true, 0.755);
        assert!((m - 1.0).abs() < 0.04, "one row ring: mean {m}");
        // Round the row's middle (0.8725 of the radius), the diodes counted where the face first rises past its mid level.
        let row: Vec<f32> = (0..1440).map(|i| {
            let a = i as f32 / 1440.0 * std::f32::consts::TAU;
            ring.factor(vec2f(0.27 * 0.8725 * a.sin(), 0.27 * 0.8725 * a.cos()))
        }).collect();
        let mid = 0.5 * (row.iter().cloned().fold(f32::MAX, f32::min) + row.iter().cloned().fold(0.0, f32::max));
        let diodes = (0..row.len()).filter(|&i| row[i] > mid && row[(i + row.len() - 1) % row.len()] <= mid).count();
        assert_eq!(diodes, 36);
        for diffusion in [0.05, 0.22, 0.6] {
            let p = ModifierParams { kind: "led_panel".to_string(), rim: 0.02, baffle: 0.5, ribs: 16, plate: 0.0, diffusion: Some(diffusion), across: None, ..ModifierParams::default() };
            let m = mean(&Modifier::new(&p, vec2f(0.27, 0.27)).unwrap(), false, 0.0);
            assert!((m - 1.0).abs() < 0.06, "tiles {diffusion}: mean {m}");
        }
    }
}

#[cfg(test)]
mod gradient_tests {
    use super::*;

    /// The gradient's lamp's settings for the twelve 3D Collective plates
    /// (RLS043-054): ribs, across, plate, diffusion, baffle.
    const PLATES: [(u32, f32, f32, f32, f32); 12] = [
        (1, -0.765, 0.604, 0.795, 0.889),
        (1, -0.837, 0.0, 0.527, 0.641),
        (1, -0.726, 0.0, 0.0, 0.0),
        (0, -0.693, 0.0, 0.593, 0.0),
        (1, -0.919, 0.735, 0.0, 0.0),
        (0, -0.994, 0.0, 0.003, 0.003),
        (1, -0.9, 0.589, 0.0, 0.0),
        (3, 0.0, 0.0, 0.24, 0.0),
        (0, 0.0, 0.0, 0.65, 0.187),
        (1, 0.0, 0.0, 0.719, 0.0),
        (1, -0.668, 0.0, 0.73, 0.306),
        (0, -0.532, 0.0, 0.204, 0.0),
    ];

    fn gradient(ribs: u32, across: f32, plate: f32, diffusion: f32, baffle: f32) -> Modifier {
        let p = ModifierParams { kind: "gradient".to_string(), rim: 0.0, baffle, ribs, plate, diffusion: Some(diffusion), across: Some(across), ..ModifierParams::default() };
        Modifier::new(&p, vec2f(0.18, 0.18)).unwrap()
    }

    /// The face over a 400 x 400 midpoint grid of the box, rows from the top.
    fn face(m: &Modifier) -> Vec<f32> {
        let n = 400;
        (0..n * n).map(|k| m.factor(vec2f((2.0 * ((k % n) as f32 + 0.5) / n as f32 - 1.0) * 0.18, (1.0 - 2.0 * ((k / n) as f32 + 0.5) / n as f32) * 0.18))).collect()
    }

    #[test]
    fn every_plates_gradient_sits_at_mean_one() {
        for (i, &(ribs, across, plate, diffusion, baffle)) in PLATES.iter().enumerate() {
            let f = face(&gradient(ribs, across, plate, diffusion, baffle));
            let mean = f.iter().sum::<f32>() / f.len() as f32;
            // The box's soft edge takes a little of a strip's light at it (RLS054's, 4 %).
            assert!((mean - 1.0).abs() < 0.05, "RLS0{}: mean {mean}", 43 + i);
        }
        // Older files (no diffusion, no across): a ramp from the left and a spot in the middle, each at mean 1.
        for ribs in [0, 1] {
            let p = ModifierParams { kind: "gradient".to_string(), rim: 0.0, baffle: 0.15, ribs, plate: 0.0, diffusion: None, across: None, ..ModifierParams::default() };
            let f = face(&Modifier::new(&p, vec2f(0.18, 0.18)).unwrap());
            let mean = f.iter().sum::<f32>() / f.len() as f32;
            assert!((mean - 1.0).abs() < 0.03, "ribs {ribs}: mean {mean}");
        }
    }

    #[test]
    fn the_lamp_stands_where_across_and_plate_put_it() {
        let at = |m: &Modifier, x: f32, y: f32| m.factor(vec2f(x * 0.18, y * 0.18));
        // RLS043: the round lamp off the lower left, its spot drawn up to the right.
        let m = gradient(1, -0.765, 0.604, 0.795, 0.889);
        assert!(at(&m, -0.7, -0.7) > 3.0 * at(&m, 0.9, -0.9) && at(&m, -0.7, -0.7) > 3.0 * at(&m, -0.9, 0.9));
        // A strip off the left edge lights the left; three strips three bands.
        let m = gradient(0, -0.532, 0.0, 0.204, 0.0);
        assert!(at(&m, -0.9, 0.0) > 50.0 * at(&m, 0.9, 0.0));
        let m = gradient(3, 0.0, 0.0, 0.24, 0.0);
        for x in [-2.0 / 3.0, 0.0, 2.0 / 3.0] {
            assert!(at(&m, x, 0.3) > 2.0 * at(&m, x + 1.0 / 3.0, 0.3));
        }
    }

    fn knobs(f: impl FnOnce(&mut ModifierParams)) -> Modifier {
        let mut p = ModifierParams { kind: "gradient".to_string(), rim: 0.0, baffle: 0.0, ribs: 0, plate: 0.0, diffusion: Some(0.425), across: Some(-0.857), ..ModifierParams::default() };
        f(&mut p);
        Modifier::new(&p, vec2f(0.18, 0.18)).unwrap()
    }

    /// The mean of a column of the face (x in box units), smooth of the creases.
    fn column(m: &Modifier, x: f32) -> f32 {
        (0..64).map(|j| m.factor(vec2f(x * 0.18, (2.0 * (j as f32 + 0.5) / 64.0 - 1.0) * 0.18))).sum::<f32>() / 64.0
    }

    #[test]
    fn a_second_lamp_brightens_the_far_side_again() {
        // RLS048: falling from the left to 0.83 at 0.7 of the width, then up to 1.09 at the right edge.
        let one = knobs(|p| p.creases = Some(0.0));
        let two = knobs(|p| {
            p.creases = Some(0.0);
            p.fill = Some(0.018);
            p.fill_across = Some(0.603);
        });
        assert!(column(&one, 0.95) < column(&one, 0.4), "one lamp falls all the way");
        assert!(column(&two, 0.95) > 1.15 * column(&two, 0.4), "{} {}", column(&two, 0.95), column(&two, 0.4));
        let mean = face(&two).iter().sum::<f32>() / 160_000.0;
        assert!((mean - 1.0).abs() < 0.05, "mean {mean}");
    }

    #[test]
    fn a_round_lamp_aimed_below_leaves_the_top_dark() {
        // SoftBox_Gradient: about 300 x brighter at the bottom than at the top.
        let m = knobs(|p| {
            p.ribs = 1;
            p.across = Some(0.0);
            p.plate = 0.427;
            p.diffusion = Some(0.8);
            p.baffle = 0.189;
            p.aim = Some(-0.741);
            p.creases = Some(0.0);
        });
        let row = |y: f32| (0..32).map(|i| m.factor(vec2f((2.0 * (i as f32 + 0.5) / 32.0 - 1.0) * 0.18, y * 0.18))).sum::<f32>() / 32.0;
        assert!(row(-0.95) > 100.0 * row(0.95), "{} {}", row(-0.95), row(0.95));
        let middle = knobs(|p| {
            p.ribs = 1;
            p.across = Some(0.0);
            p.plate = 0.427;
            p.diffusion = Some(0.8);
            p.baffle = 0.189;
            p.creases = Some(0.0);
        });
        let top = |m: &Modifier| m.factor(vec2f(0.0, 0.95 * 0.18));
        assert!(top(&m) < 0.2 * top(&middle), "aimed at the middle the top is lit: {} {}", top(&m), top(&middle));
    }

    #[test]
    fn the_scrims_creases_are_texture_only() {
        // Creased, the face keeps its level (the folds' slopes average out) and shows folds where a smooth one shows none.
        let smooth = knobs(|p| p.creases = Some(0.0));
        let creased = knobs(|_| {});
        let (a, b) = (face(&smooth), face(&creased));
        let (ma, mb) = (a.iter().sum::<f32>() / a.len() as f32, b.iter().sum::<f32>() / b.len() as f32);
        assert!((ma - mb).abs() < 0.01, "{ma} {mb}");
        let most = a.iter().zip(&b).map(|(x, y)| (y / x - 1.0).abs()).fold(0.0f32, f32::max);
        assert!(most > 0.02 && most < 0.3, "the folds move the face by {most}");
    }
}
