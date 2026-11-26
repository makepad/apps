//! The moon's face: its albedo and its relief, generated with no texture (the owner's decision of 2026-11-09).
//!
//! **Where.** Selenographic coordinates on the unit sphere, in the body frame: x toward the IAU east (Mare Crisium's
//! limb), y north, z toward the Earth. The near side faces the Earth with no libration: the sub-Earth point is latitude
//! 0, longitude 0. The face is worked out once per process into a pyramid over the disc as the Earth sees it
//! (orthographic, x east and y north from -1 to 1), FACE_SIZE texels across and its mip levels down to one texel
//! ([`MoonFace`]). The map's moon texels (`night.rs`) and the app's preview disc read the same pyramid, each filtered
//! over its own footprint, so the face holds from about 12 to 230 texels across without aliasing or shimmer.
//!
//! **Albedo**, relative: its mean over the disc is 1, so the moon's whole light stays what Allen's law gives it.
//! - The maria sit where the real ones are (MARIA: the IAU gazetteer's centres and diameters; Oceanus Procellarum,
//!   Mare Frigoris and the irregular maria as chains of patches). Their coasts are where lava filling the basins would
//!   stand: each patch is a depression whose wall is as soft as the mare is irregular, the highlands' roughness is
//!   added at several scales, and the mare is where the sum passes the flood level. A basin's coast (Crisium,
//!   Serenitatis) is then a smooth arc with small bays, and an irregular mare's (Tranquillitatis, Procellarum) lobed at
//!   every scale, with islands and ponds near its edge and a fringe of mixed soil grading out past it.
//! - Each patch has the real Moon's tone there (its mean albedo over the patch's inner half on a real albedo map,
//!   against the disc's mean, rounded), from Procellarum's west and Tranquillitatis' north-west, the darkest, to
//!   Frigoris, Palus Somni, Sinus Medii and Insularum, little darker than the highlands round them; Serenitatis lighter
//!   with a darker rim. The colour is bluest on the dark titanium-rich basalts (Tranquillitatis, Procellarum's west),
//!   blended where patches meet, with lava flows of a slightly different tone within each.
//! - The highlands are brighter (published normal albedos: maria 0.07, highlands 0.12 to 0.16), mottled at several
//!   scales, and darker toward the maria over a few hundred km (NEAR_MARE_DARK), so the face grades from the maria to
//!   the bright southern highlands as the real near side does rather than stepping from dark to light at every coast;
//!   saturated with craters (the crater field below), fresh ones bright with a halo of ejecta.
//! - Tycho, Copernicus, Kepler and Aristarchus are ray craters (RAY_CRATERS): a bright crater and halo, and many thin
//!   radial rays, faint and beaded with gaps along their length, so they read as a feathery splash and not as spokes.
//!
//! **Relief**: the surface's slope, which `night.rs` bends the sphere's normal by. A random crater field with sizes on a
//! power law (each octave of diameter four times as many per area as the one above, so N(>D) ~ D^-2), bowls with
//! raised rims, flat floors and central peaks on the large ones (Pike's depth and rim height for their diameter), on
//! the maria only the young ones; and the large named craters the terminator is known by (NAMED: Ptolemaeus,
//! Theophilus, Clavius...), each as fresh or worn as it looks. The light law decides where it shows: along the
//! terminator, where the light grazes, and nowhere at full moon or on the night side.
//!
//! **Cost.** The pyramid is built once per process, the first time a sky draws a moon: about 2 s of one core, a
//! quarter of a second on 16 (FACE_SIZE^2 texels, each the maria, the crater octaves' 3 x 3 x 3 cells and the ray
//! craters' sectors), 5.6 MB. After that a lookup on the moon is two bilinear reads of the pyramid; nothing off it.

use super::noise::{hash3_u32, hash_u32, perlin3};
use std::f32::consts::{PI, TAU};
use std::sync::{Arc, OnceLock};

/// Texels across the finest level of the face, over the disc's diameter: 6.8 km at the disc's centre, twice as fine as
/// the largest moon a map draws (10 degrees in an 8K map is 228 texels across) and fine enough for the preview's zoom.
pub const FACE_SIZE: usize = 512;
/// Levels of the pyramid, FACE_SIZE texels across down to one.
pub const FACE_LEVELS: usize = 10;
/// The atlas a GPU reads the pyramid from ([`MoonFace::atlas`]): level 0 at the left, the others stacked down the
/// right-hand column, level k (k >= 1) at x FACE_SIZE, y FACE_SIZE - 2 (FACE_SIZE >> k).
pub const ATLAS_WIDTH: usize = FACE_SIZE + FACE_SIZE / 2;
pub const ATLAS_HEIGHT: usize = FACE_SIZE;
/// The colour an albedo's colour index adds per channel (R, G, B), its luminance 0: index 1 is about 8 % bluer in B/R
/// than index 0, the spread between the bluest maria (Tranquillitatis) and the reddest (Serenitatis, Imbrium's east).
pub const COLOR_AXIS: [f32; 3] = [-0.03, 0.003_87, 0.05];

/// The Moon's mean radius (km).
const R_MOON_KM: f32 = 1737.4;
/// The face is evaluated out to this radius of the disc and held there beyond, so the coarse levels average the limb
/// with itself and not with black.
const LIMB_REACH: f32 = 0.995;

/// A patch of mare: its centre (selenographic latitude, east longitude, degrees) and diameter (km), as the IAU
/// gazetteer gives them for a single mare, or a piece of a chain; how soft its wall is (a fraction of its radius: a
/// basin's coast is sharp, an irregular mare's lobed); its tone (against the highlands' 1), the tone toward its rim
/// (Serenitatis' dark border), and its colour index (1 bluest).
struct Mare {
    lat: f32,
    lon: f32,
    km: f32,
    soft: f32,
    tone: f32,
    rim_tone: f32,
    color: f32,
}

const fn mare(lat: f32, lon: f32, km: f32, soft: f32, tone: f32, color: f32) -> Mare {
    Mare { lat, lon, km, soft, tone, rim_tone: tone, color }
}

/// The near side's maria. Single maria at the gazetteer's centre and diameter; the irregular ones as chains of patches
/// along their length (their gazetteer diameter is the chain's span), overlapping where the real ones join.
const MARIA: &[Mare] = &[
    // Mare Imbrium (32.8 N, 15.6 W, 1146 km): a basin, sharp to the east and north, open to Procellarum in the west;
    // its younger south-western flows darker and bluer.
    mare(33.0, -17.0, 1180.0, 0.08, 0.54, -0.3),
    mare(27.0, -27.0, 450.0, 0.42, 0.48, 0.6),
    // Oceanus Procellarum (18.4 N, 57.4 W, 2568 km): a chain north to south down the western near side, joined to
    // Imbrium and Insularum.
    mare(47.0, -47.0, 560.0, 0.42, 0.70, 0.1),
    mare(36.0, -57.0, 680.0, 0.42, 0.52, 0.7),
    mare(24.0, -60.0, 760.0, 0.42, 0.45, 0.6),
    mare(11.0, -57.0, 760.0, 0.42, 0.44, 0.6),
    mare(0.0, -52.0, 700.0, 0.42, 0.44, 0.5),
    mare(-8.0, -44.0, 520.0, 0.47, 0.49, 0.4),
    mare(22.0, -45.0, 520.0, 0.42, 0.58, 0.7),
    mare(12.0, -40.0, 450.0, 0.47, 0.52, 0.6),
    // Mare Insularum (7.5 N, 30.9 W, 513 km), Mare Cognitum (10.0 S, 23.1 W, 376 km).
    mare(8.0, -28.0, 520.0, 0.47, 0.65, 0.1),
    // The mare round Copernicus, between Imbrium's southern shore (the Carpathians) and Insularum.
    mare(15.0, -18.0, 420.0, 0.30, 0.52, 0.5),
    mare(-10.0, -22.0, 420.0, 0.47, 0.49, 0.2),
    // Mare Nubium (21.3 S, 16.6 W, 715 km): irregular; joined to Humorum through Palus Epidemiarum's side.
    mare(-20.0, -15.0, 600.0, 0.42, 0.51, 0.1),
    mare(-14.0, -7.0, 320.0, 0.35, 0.63, 0.1),
    mare(-27.0, -22.0, 360.0, 0.52, 0.57, 0.1),
    mare(-17.0, -30.0, 380.0, 0.52, 0.53, 0.2),
    // Mare Humorum (24.4 S, 38.6 W, 389 km), Palus Epidemiarum (32.0 S, 28.2 W, 286 km).
    mare(-24.4, -38.6, 400.0, 0.08, 0.47, 0.3),
    mare(-32.0, -28.0, 260.0, 0.35, 0.60, 0.1),
    // Sinus Aestuum (10.9 N, 8.8 W, 290 km, a dark mantle), Sinus Medii (2.4 N, 1.7 E, 335 km), Mare Vaporum (13.3 N,
    // 3.6 E, 245 km), Palus Putredinis (27.4 N, 0.4 E, 161 km).
    mare(11.0, -8.0, 300.0, 0.35, 0.57, 0.4),
    mare(2.4, 1.7, 340.0, 0.30, 0.83, 0.0),
    mare(14.0, 4.0, 320.0, 0.25, 0.58, 0.3),
    mare(27.0, 0.0, 180.0, 0.35, 0.72, 0.0),
    // Mare Serenitatis (28.0 N, 17.5 E, 707 km): a basin, lighter in the middle with a darker, bluer rim.
    Mare { lat: 28.0, lon: 17.5, km: 710.0, soft: 0.07, tone: 0.55, rim_tone: 0.47, color: -0.6 },
    // Mare Tranquillitatis (8.5 N, 31.4 E, 873 km): irregular, the darkest and bluest of the large maria, joined to
    // Serenitatis, Fecunditatis and (through Sinus Asperitatis) Nectaris.
    mare(9.0, 25.0, 540.0, 0.42, 0.45, 1.0),
    mare(13.0, 35.0, 440.0, 0.42, 0.47, 1.0),
    mare(3.0, 33.0, 480.0, 0.42, 0.59, 0.5),
    mare(0.0, 42.0, 320.0, 0.35, 0.62, 0.3),
    mare(19.0, 24.0, 360.0, 0.25, 0.47, 0.9),
    mare(-4.0, 26.0, 260.0, 0.35, 0.67, 0.2),
    // Mare Crisium (17.0 N, 59.1 E, 556 km): a sharp oval basin.
    mare(17.0, 59.1, 540.0, 0.06, 0.55, 0.2),
    // Mare Fecunditatis (7.8 S, 51.3 E, 909 km): irregular, along the meridian.
    mare(-1.0, 50.0, 440.0, 0.42, 0.47, 0.4),
    mare(-9.0, 52.0, 470.0, 0.42, 0.53, 0.4),
    mare(-17.0, 50.0, 330.0, 0.35, 0.81, 0.3),
    // Mare Nectaris (15.2 S, 35.5 E, 333 km).
    mare(-15.2, 35.5, 350.0, 0.10, 0.65, 0.1),
    // Lacus Somniorum (38.0 N, 29.2 E, 384 km), Lacus Mortis (45.0 N, 27.2 E, 151 km), Palus Somni (14.1 N, 45.0 E,
    // 143 km).
    mare(37.0, 30.0, 340.0, 0.35, 0.76, 0.0),
    mare(45.0, 27.0, 150.0, 0.35, 0.82, 0.0),
    mare(14.0, 45.0, 150.0, 0.35, 0.84, 0.2),
    // Sinus Roris (54.0 N, 56.6 W, 202 km) joins Procellarum to Mare Frigoris (56.0 N, 1.4 E, 1596 km), a thin band
    // along the north; Sinus Iridum (44.1 N, 31.5 W, 236 km), Imbrium's bay.
    mare(53.0, -52.0, 300.0, 0.35, 0.62, 0.1),
    mare(57.0, -38.0, 300.0, 0.35, 0.64, -0.2),
    mare(57.0, -22.0, 300.0, 0.35, 0.71, -0.2),
    mare(56.0, -7.0, 300.0, 0.35, 0.89, -0.3),
    mare(56.0, 8.0, 300.0, 0.35, 0.83, -0.3),
    mare(57.0, 22.0, 280.0, 0.35, 0.75, -0.3),
    mare(58.0, 36.0, 250.0, 0.35, 0.65, -0.3),
    mare(59.0, 47.0, 200.0, 0.35, 0.66, -0.2),
    mare(44.1, -31.5, 250.0, 0.12, 0.56, 0.0),
    // Near the eastern limb: Mare Smythii (1.3 N, 87.5 E, 373 km), Mare Marginis (13.3 N, 86.1 E, 420 km), Mare
    // Undarum (6.8 N, 68.4 E, 243 km), Mare Spumans (1.1 N, 65.1 E, 139 km), Mare Humboldtianum (56.8 N, 81.5 E,
    // 273 km); Mare Australe (38.9 S, 93.0 E, 603 km) on the south-eastern limb, in patches.
    mare(1.3, 87.5, 360.0, 0.42, 0.62, 0.2),
    mare(13.3, 86.1, 380.0, 0.57, 0.66, 0.2),
    mare(6.8, 68.4, 220.0, 0.35, 0.63, 0.2),
    mare(1.1, 65.1, 140.0, 0.35, 0.63, 0.2),
    mare(56.8, 81.5, 270.0, 0.35, 0.63, 0.0),
    mare(-40.0, 82.0, 320.0, 0.35, 0.70, 0.0),
    mare(-47.0, 90.0, 280.0, 0.35, 0.70, 0.0),
    // Dark-floored craters: Plato (51.6 N, 9.4 W, 101 km), Grimaldi (5.2 S, 68.6 W, 173 km), Riccioli (3.3 S,
    // 74.6 W, 139 km), Endymion (53.6 N, 57.0 E, 123 km).
    mare(51.6, -9.4, 100.0, 0.10, 0.42, 0.3),
    mare(-5.2, -68.6, 160.0, 0.12, 0.48, 0.4),
    mare(-3.3, -74.6, 120.0, 0.15, 0.55, 0.3),
    mare(53.6, 57.0, 115.0, 0.12, 0.55, 0.2),
];

/// A ray crater: its centre (latitude, east longitude), diameter (km), how far its longest rays reach (km), how many
/// rays it throws, their strength, and a dark collar of impact melt round the rim (Tycho's).
struct RayCrater {
    lat: f32,
    lon: f32,
    km: f32,
    reach_km: f32,
    rays: u32,
    strength: f32,
    collar: f32,
}

/// Tycho (43.31 S, 11.36 W, 85 km), Copernicus (9.62 N, 20.08 W, 96 km), Kepler (8.12 N, 38.01 W, 30 km) and
/// Aristarchus (23.73 N, 47.49 W, 40 km, the brightest spot on the moon). Their rays as photographs of the full moon
/// show them: Tycho's across most of the southern highlands and on into the maria, Copernicus' over Imbrium and
/// Procellarum, Kepler's a smaller star, Aristarchus' short.
const RAY_CRATERS: [RayCrater; 4] = [
    RayCrater { lat: -43.31, lon: -11.36, km: 85.0, reach_km: 1700.0, rays: 220, strength: 1.0, collar: 0.15 },
    RayCrater { lat: 9.62, lon: -20.08, km: 96.0, reach_km: 650.0, rays: 220, strength: 0.65, collar: 0.0 },
    RayCrater { lat: 8.12, lon: -38.01, km: 30.0, reach_km: 400.0, rays: 140, strength: 0.55, collar: 0.0 },
    RayCrater { lat: 23.73, lon: -47.49, km: 40.0, reach_km: 220.0, rays: 90, strength: 0.7, collar: 0.0 },
];

/// A large named crater: its centre (latitude, east longitude), diameter (km) and freshness (0 worn to its floor, 1 as
/// sharp as Tycho), as the gazetteer and photographs of the terminator show them.
struct Named {
    lat: f32,
    lon: f32,
    km: f32,
    fresh: f32,
}

const fn named(lat: f32, lon: f32, km: f32, fresh: f32) -> Named {
    Named { lat, lon, km, fresh }
}

/// The large craters the terminator is known by: the four ray craters' relief, the chains down the middle
/// (Ptolemaeus, Alphonsus, Arzachel; Albategnius, Hipparchus), Theophilus, Cyrillus and Catharina by Nectaris, the
/// southern highlands' old giants (Clavius, Maginus, Deslandres) and the eastern limb's (Petavius, Langrenus).
const NAMED: &[Named] = &[
    named(-43.31, -11.36, 85.0, 1.0),
    named(9.62, -20.08, 96.0, 0.95),
    named(8.12, -38.01, 30.0, 0.9),
    named(23.73, -47.49, 40.0, 1.0),
    named(-9.16, -1.84, 154.0, 0.15),
    named(-13.39, -3.19, 119.0, 0.35),
    named(-18.26, -1.93, 97.0, 0.6),
    named(-11.24, 4.01, 136.0, 0.3),
    named(-5.11, 4.82, 150.0, 0.1),
    named(-33.0, 0.7, 132.0, 0.25),
    named(-25.5, -1.9, 118.0, 0.25),
    named(-32.5, -5.2, 256.0, 0.05),
    named(-58.62, -14.73, 231.0, 0.3),
    named(-50.0, -6.2, 194.0, 0.15),
    named(-49.6, -21.7, 145.0, 0.3),
    named(-44.3, -54.6, 227.0, 0.15),
    named(-17.6, -40.1, 110.0, 0.3),
    named(-11.4, 26.4, 110.0, 0.85),
    named(-13.2, 24.0, 98.0, 0.45),
    named(-18.0, 23.6, 104.0, 0.3),
    named(-25.1, 60.4, 177.0, 0.4),
    named(-8.9, 61.1, 132.0, 0.75),
    named(-16.3, 61.6, 147.0, 0.2),
    named(-44.9, 41.0, 190.0, 0.1),
    named(-41.1, 6.0, 126.0, 0.25),
    named(-42.0, 14.0, 114.0, 0.3),
    named(31.9, 29.9, 95.0, 0.2),
    named(29.7, -4.0, 81.0, 0.35),
    named(14.5, -11.3, 58.0, 0.7),
    named(50.2, 17.4, 87.0, 0.6),
    named(44.3, 16.3, 67.0, 0.55),
    named(-20.7, -22.2, 61.0, 0.8),
    named(-29.8, -13.5, 106.0, 0.2),
    named(-21.5, 33.0, 124.0, 0.15),
    named(-29.7, 32.2, 88.0, 0.5),
    named(-36.0, 60.6, 125.0, 0.25),
    named(46.7, 44.4, 87.0, 0.5),
    named(46.7, 39.1, 69.0, 0.45),
    named(27.6, 55.5, 126.0, 0.3),
    named(39.2, 60.5, 125.0, 0.2),
];

/// The highlands' albedo against the maria's tones above (1), and how bright a ray's material is against them.
const HIGHLAND: f32 = 1.0;
const RAY_ALBEDO: f32 = 1.5;
/// The near side's highlands brighten with the distance from the maria's coasts over a few hundred km (the basins'
/// mafic ejecta, the maria buried under it, and the coasts' lateral mixing): against the far highlands, about 0.76
/// within 50 km of a coast, 0.82 at 75 km, 0.89 at 150 km and 0.95 at 300 km on the real Moon. Darkest at a coast by
/// this much, falling off over this distance (km).
const NEAR_MARE_DARK: f32 = 0.28;
const NEAR_MARE_KM: f32 = 180.0;
/// The bend of the maria's outlines (`maria`), in the moon's radii: at a few hundred km and at a few tens.
const WARP: f32 = 0.2;
const WARP_FINE: f32 = 0.04;

/// The random crater field: the largest octave's radius (km; its craters are 80 to 160 km across), the octaves (each
/// half the radius of the one before, down to 2.5 km across), and the cell of each octave's grid in crater radii (a
/// cell holds at most one crater, so a crater's reach, 2.5 of its radii at most 2 of the octave's, stays within the
/// 3 x 3 x 3 cells round a point). A full octave covers about a quarter of the surface; the largest octaves are held to
/// CRATER_OCCUPANCY of their cells, so the near side's highlands hold about 80 craters over 80 km and a few over 160,
/// as the lunar highlands' counts (N(>20 km) about 1.5e-4 per km^2, falling as D^-2.2), and are saturated from 20 km
/// down.
const CRATER_R0_KM: f32 = 40.0;
const CRATER_OCTAVES: u32 = 6;
const CRATER_CELL: f32 = 5.0;
/// The share of the largest octave's cells that hold a crater, growing by half per octave to all of them.
const CRATER_OCCUPANCY: f32 = 0.3;
/// How far a crater's relief and halo reach, in its radii.
const CRATER_REACH: f32 = 2.5;
/// A crater's freshness is a uniform number to a power: most craters are worn, a few fresh and bright with ejecta. The
/// power is this for craters 10 to 20 km across (one in twenty over one half, one in fifty over 0.8), higher for the
/// larger ones (fresh large craters are rare: Copernicus, Tycho) and lower for the smaller.
const FRESH_POWER: f32 = 12.0;
/// A crater is young enough to stand on the maria above this freshness (about one in ten).
const POST_MARE: f32 = 0.3;

/// A point or direction in the body frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct V3 {
    x: f32,
    y: f32,
    z: f32,
}

impl V3 {
    const fn new(x: f32, y: f32, z: f32) -> V3 {
        V3 { x, y, z }
    }
    fn dot(self, o: V3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    fn cross(self, o: V3) -> V3 {
        V3::new(self.y * o.z - self.z * o.y, self.z * o.x - self.x * o.z, self.x * o.y - self.y * o.x)
    }
    fn length(self) -> f32 {
        self.dot(self).sqrt()
    }
    fn normalize(self) -> V3 {
        let l = self.length();
        if l > 0.0 { self * (1.0 / l) } else { self }
    }
}

impl std::ops::Add for V3 {
    type Output = V3;
    fn add(self, o: V3) -> V3 {
        V3::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}
impl std::ops::Sub for V3 {
    type Output = V3;
    fn sub(self, o: V3) -> V3 {
        V3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}
impl std::ops::Mul<f32> for V3 {
    type Output = V3;
    fn mul(self, s: f32) -> V3 {
        V3::new(self.x * s, self.y * s, self.z * s)
    }
}
impl std::ops::AddAssign for V3 {
    fn add_assign(&mut self, o: V3) {
        *self = *self + o;
    }
}

use crate::patterns::smooth as smoothstep;

/// The unit vector at selenographic latitude and east longitude (degrees).
fn selenographic(lat: f32, lon: f32) -> V3 {
    let (sl, cl) = lat.to_radians().sin_cos();
    let (so, co) = lon.to_radians().sin_cos();
    V3::new(cl * so, sl, cl * co)
}

/// The chord between two points of the unit sphere `km` apart along its surface.
fn chord_km(km: f32) -> f32 {
    2.0 * (0.5 * km / R_MOON_KM).sin()
}

/// The top 24 bits of a hash as a float in [0, 1).
use crate::noise::unit;

/// How much of a detail of wavelength `wavelength` a lookup `footprint` wide keeps (both as arcs of the unit sphere):
/// all of it from two footprints a wavelength up, none under one (Nyquist's limit; the pyramid's levels filter the
/// rest).
fn keep(wavelength: f32, footprint: f32) -> f32 {
    smoothstep(1.0, 2.0, wavelength / footprint.max(1.0e-9))
}

/// Gradient noise summed over `octaves` from `freq` (cycles per unit of the sphere's radius) up, each octave 2.03 times
/// finer and `gain` times weaker, each faded out as `footprint` reaches it ([`keep`]). Normalised by every octave's
/// amplitude, so what the footprint leaves out lowers the variance and does not stretch the rest. About -1..1.
fn fbm(p: V3, freq: f32, octaves: u32, gain: f32, seed: u32, footprint: f32) -> f32 {
    let (mut f, mut a, mut sum, mut total) = (freq, 1.0f32, 0.0f32, 0.0f32);
    for o in 0..octaves {
        total += a;
        let k = keep(1.0 / f, footprint);
        if k > 0.0 {
            sum += a * k * perlin3(p.x * f + 0.37 * o as f32, p.y * f - 0.71 * o as f32, p.z * f + 0.13 * o as f32, crate::noise::octave_seed(seed, o));
        }
        f *= 2.03;
        a *= gain;
    }
    sum / total
}

/// A mare patch prepared: its centre, chord radius, wall softness, tones and colour.
struct Patch {
    c: V3,
    r: f32,
    /// What the bend moves the patch's middle by, taken back for a small patch (`maria`).
    shift: V3,
    soft: f32,
    tone: f32,
    rim_tone: f32,
    color: f32,
}

/// A ray crater prepared: its centre, an east and a north at it, its radius (km).
struct Rays {
    c: V3,
    east: V3,
    north: V3,
    radius_km: f32,
    reach_km: f32,
    cos_reach: f32,
    rays: u32,
    strength: f32,
    collar: f32,
    seed: u32,
}

/// What the face is built from, prepared once.
struct Prepared {
    patches: Vec<Patch>,
    /// The named craters' centres, angular radii and freshness.
    named: Vec<(V3, f32, f32)>,
    rays: Vec<Rays>,
}

impl Prepared {
    fn new() -> Prepared {
        let patches = MARIA
            .iter()
            .map(|m| {
                let (c, r) = (selenographic(m.lat, m.lon), chord_km(0.5 * m.km));
                let hold = 1.0 - smoothstep(0.1, 0.2, r);
                let shift = (warp_at(c, 0.0) * WARP + fine_warp_at(c, 0.0) * WARP_FINE) * hold;
                Patch { c, r, shift, soft: m.soft, tone: m.tone, rim_tone: m.rim_tone, color: m.color }
            })
            .collect();
        let rays = RAY_CRATERS
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let c = selenographic(r.lat, r.lon);
                let east = V3::new(0.0, 1.0, 0.0).cross(c).normalize();
                let north = c.cross(east);
                Rays {
                    c,
                    east,
                    north,
                    radius_km: 0.5 * r.km,
                    reach_km: r.reach_km,
                    cos_reach: (r.reach_km / R_MOON_KM).cos(),
                    rays: r.rays,
                    strength: r.strength,
                    collar: r.collar,
                    seed: hash_u32(0x5241_5953 ^ i as u32),
                }
            })
            .collect();
        let named = NAMED.iter().map(|c| (selenographic(c.lat, c.lon), chord_km(0.5 * c.km), c.fresh)).collect();
        Prepared { patches, named, rays }
    }
}

/// The surface at a point: its albedo, colour index and slope (the height's gradient along the surface, a tangent
/// vector, height in units of the moon's radius).
struct Surface {
    albedo: f32,
    color: f32,
    slope: V3,
}

/// The bend of the maria's outlines at `p` (`maria`): at a few hundred km, and at a few tens.
fn warp_at(p: V3, footprint: f32) -> V3 {
    V3::new(fbm(p, 2.6, 3, 0.5, 11, footprint), fbm(p, 2.6, 3, 0.5, 12, footprint), fbm(p, 2.6, 3, 0.5, 13, footprint))
}

fn fine_warp_at(p: V3, footprint: f32) -> V3 {
    V3::new(fbm(p, 9.0, 2, 0.5, 14, footprint), fbm(p, 9.0, 2, 0.5, 15, footprint), fbm(p, 9.0, 2, 0.5, 16, footprint))
}

/// The maria at `p`, filtered over `footprint`: how much is mare (0..1), its tone and colour index, and how far `p`
/// lies past the nearest patch's edge (km; negative inside one).
fn maria(prep: &Prepared, p: V3, footprint: f32) -> (f32, f32, f32, f32) {
    // The basins' outlines bent at the scale of a few hundred km and again at a few tens, so no patch stays a circle
    // and the chains' circles do not show. The bend would move a small patch (a dark crater floor, Vaporum) off the
    // gazetteer's place by most of its size: its middle is held there (`Patch::shift`) and only its outline bends.
    let warp = warp_at(p, footprint);
    let fine = fine_warp_at(p, footprint);
    let bent = p + warp * WARP + fine * WARP_FINE;
    // The depression the patches make (a soft union), and the tones weighed by how deep each patch is there.
    let (mut out, mut tone, mut color, mut weight) = (1.0f32, 0.0f32, 0.0f32, 0.0f32);
    let mut edge = f32::MAX;
    for patch in &prep.patches {
        let q = bent - patch.shift;
        let d = (q - patch.c).length() / patch.r;
        edge = edge.min((d - 1.0) * patch.r);
        if d >= 1.0 + patch.soft {
            continue;
        }
        let b = smoothstep(1.0 + patch.soft, 1.0 - patch.soft, d);
        out *= 1.0 - b;
        let w = b * b + 1.0e-4;
        let t = patch.tone + (patch.rim_tone - patch.tone) * smoothstep(0.55, 0.95, d);
        tone += w * t;
        color += w * patch.color;
        weight += w;
    }
    let edge_km = edge * R_MOON_KM;
    if weight <= 0.0 {
        return (0.0, HIGHLAND, 0.0, edge_km);
    }
    let depression = 1.0 - out;
    // The flood: the highlands' roughness at several scales added to the depression, the mare where the sum passes
    // the flood level (a little under half: a patch's mare reaches a little past its gazetteer diameter). Its coast is
    // sharp, with a fringe of mixed soil grading out past it over a few tens of km.
    let rough = fbm(p, 2.5, 8, 0.6, 21, footprint);
    let level = depression + 0.65 * rough;
    let mare = 0.7 * smoothstep(0.37, 0.47, level) + 0.3 * smoothstep(0.2, 0.55, level);
    let (tone, color) = (tone / weight, color / weight);
    // Lava flows: lobes of a slightly different tone over a few hundred km, and the fine mottling of the mare's surface.
    let broad = fbm(p, 3.5, 3, 0.5, 35, footprint);
    let flows = fbm(p + warp * 0.08, 9.0, 4, 0.55, 31, footprint);
    let flow_units = smoothstep(-0.12, 0.12, flows) * 2.0 - 1.0;
    let mottle = fbm(p, 40.0, 3, 0.6, 33, footprint);
    // Near its coast a mare is a little lighter, its thin basalt mixed with the highlands' ejecta.
    let shore = 1.0 - smoothstep(0.42, 0.75, level);
    let tone = tone * (1.0 + 0.14 * broad + 0.09 * flow_units + 0.05 * mottle + 0.08 * shore);
    let color = color + 0.25 * flow_units;
    (mare, tone, color, edge_km)
}

/// The highlands' albedo at `p` before their craters: mottled at several hundred km, at a hundred and at a few tens,
/// and darker toward the maria, `edge_km` past the nearest one's edge (NEAR_MARE_DARK, NEAR_MARE_KM).
fn highlands(p: V3, edge_km: f32, footprint: f32) -> f32 {
    let large = fbm(p, 2.6, 3, 0.55, 41, footprint);
    let middle = fbm(p, 8.0, 3, 0.55, 42, footprint);
    let fine = fbm(p, 19.0, 4, 0.6, 43, footprint);
    let near = (-edge_km.max(0.0) / NEAR_MARE_KM).exp();
    HIGHLAND * (1.0 + 0.22 * large) * (1.0 + 0.12 * middle) * (1.0 + 0.06 * fine) * (1.0 - NEAR_MARE_DARK * near)
}

/// One crater's depth profile, in units of its depth, and its slope along its radius: `t` is the distance from its
/// centre in its radii, `floor` the share of its radius that is flat floor, `rim` and `peak` the rim's and the central
/// peak's heights against the depth. A bowl rising to a sharp rim, the rim's outer flank falling to nothing at
/// CRATER_REACH.
fn crater_profile(t: f32, floor: f32, rim: f32, peak: f32) -> (f32, f32) {
    if t < floor {
        // The floor, with a central peak on a large crater.
        let s = t / 0.22;
        if peak > 0.0 && s < 1.0 {
            let u = 1.0 - s * s;
            (-1.0 + peak * u * u, peak * 2.0 * u * (-2.0 * s) / 0.22)
        } else {
            (-1.0, 0.0)
        }
    } else if t < 1.0 {
        let u = (t - floor) / (1.0 - floor);
        let s = u * u * (3.0 - 2.0 * u);
        (-1.0 + (1.0 + rim) * s, (1.0 + rim) * 6.0 * u * (1.0 - u) / (1.0 - floor))
    } else if t < CRATER_REACH {
        let u = 1.0 - (t - 1.0) / (CRATER_REACH - 1.0);
        (rim * u * u * u, -3.0 * rim * u * u / (CRATER_REACH - 1.0))
    } else {
        (0.0, 0.0)
    }
}

/// One crater centred at `c`, `d` from `p` (chords of the unit sphere), of radius `r`, its freshness `fresh` and the
/// share of it that stands above the maria (`stands`): adds its slope to `slope`, faded as the footprint reaches its
/// walls, and returns its albedo factor (a fresh crater bright inside and in a halo of ejecta, a worn one's floor a
/// little darker and its rim a little brighter; under a footprint its spot spreads over it, its light kept).
fn crater(p: V3, c: V3, d: f32, r: f32, fresh: f32, footprint: f32, stands: f32, slope: &mut V3) -> f32 {
    let t = d / r;
    let km = 2.0 * r * R_MOON_KM;
    // Pike (1977): a simple bowl to 15 km, flat-floored and peaked above; depth and rim for the diameter, a worn
    // crater shallower and lower.
    let (depth_km, rim_km, floor, peak) = if km < 15.0 {
        (0.2 * km, 0.036 * km, 0.0, 0.0)
    } else {
        let floor = smoothstep(15.0, 45.0, km) * 0.5;
        let peak = if km > 26.0 { 0.35 * (0.3 + 0.7 * fresh) } else { 0.0 };
        (1.044 * km.powf(0.301), 0.236 * km.powf(0.399), floor, peak)
    };
    let wear = 0.35 + 0.65 * fresh.max(0.6 * hash_frac(c));
    let depth = depth_km / R_MOON_KM * wear;
    let rim = rim_km / depth_km;
    let lod = smoothstep(0.7, 1.6, r * (1.0 - 0.5 * floor) / footprint.max(1.0e-9));
    if lod > 0.0 && d > 1.0e-7 {
        let (_, dh) = crater_profile(t, floor, rim, peak);
        let away = (p * p.dot(c) - c).normalize();
        *slope += away * (dh * depth / r * lod * stands);
    }
    let spread = r.max(0.7 * footprint);
    let dim = (r / spread) * (r / spread);
    let ts = d / spread;
    let inside = 1.0 - smoothstep(0.85, 1.15, ts);
    let halo = (-((ts - 1.0).max(0.0) / 0.8).powi(2)).exp() * (1.0 - smoothstep(2.0, CRATER_REACH, ts));
    let ring = (-((ts - 1.0) / 0.15).powi(2)).exp();
    let worn = (1.0 - fresh) * smoothstep(4.0, 15.0, km);
    let bright = fresh * (0.15 * inside + 0.09 * halo) + worn * (0.04 * ring - 0.04 * (1.0 - smoothstep(0.5, 0.9, ts)));
    1.0 + bright * dim * stands
}

/// A number in [0, 1) fixed by a crater's centre.
fn hash_frac(c: V3) -> f32 {
    unit(hash3_u32((c.x * 1.0e5) as i32, (c.y * 1.0e5) as i32, (c.z * 1.0e5) as i32, 0x5745_4152))
}

/// The named craters at `p`: their slopes added to `slope`, their albedo factor returned.
fn named_craters(prep: &Prepared, p: V3, footprint: f32, slope: &mut V3) -> f32 {
    let mut albedo = 1.0f32;
    for &(c, r, fresh) in &prep.named {
        let d = (p - c).length();
        if d < CRATER_REACH * r {
            albedo *= crater(p, c, d, r, fresh, footprint, 1.0, slope);
        }
    }
    albedo
}

/// The random crater field at `p`: adds each crater's slope (times `relief`, which the maria take down for the
/// craters older than them) to `slope`, and returns its albedo factor.
fn craters(p: V3, footprint: f32, mare: f32, slope: &mut V3) -> f32 {
    let mut albedo = 1.0f32;
    for k in 0..CRATER_OCTAVES {
        let radius_km = CRATER_R0_KM / (1u32 << k) as f32;
        let r0 = radius_km / R_MOON_KM;
        // Craters far under the footprint add nothing a texel can show.
        if 2.0 * r0 < 0.25 * footprint {
            break;
        }
        let cell = CRATER_CELL * r0;
        let (cx, cy, cz) = ((p.x / cell).floor() as i32, (p.y / cell).floor() as i32, (p.z / cell).floor() as i32);
        let seed = 0x4352_4154 ^ k.wrapping_mul(0x9e37_79b9);
        let occupancy = (CRATER_OCCUPANCY * 1.5f32.powi(k as i32)).min(1.0);
        for dz in -1..=1 {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (ix, iy, iz) = (cx + dx, cy + dy, cz + dz);
                    let h = hash3_u32(ix, iy, iz, seed);
                    let h2 = hash_u32(h ^ 0x68e3_1da4);
                    let h3 = hash_u32(h ^ 0xb529_7a4d);
                    let at = V3::new((ix as f32 + unit(h)) * cell, (iy as f32 + unit(h2)) * cell, (iz as f32 + unit(h3)) * cell);
                    // Only cells' points in a shell a cell thick round the sphere hold a crater: an even field on it.
                    let len = at.length();
                    if (len - 1.0).abs() > 0.5 * cell {
                        continue;
                    }
                    let c = at * (1.0 / len);
                    let h4 = hash_u32(h ^ 0x1b87_3593);
                    let h5 = hash_u32(h ^ 0xcc9e_2d51);
                    if unit(hash_u32(h ^ 0x9e37_79b9)) >= occupancy {
                        continue;
                    }
                    // Its radius within the octave, and its freshness: most are old and worn, a few fresh.
                    let r = r0 * (1.0 + unit(h4));
                    let d = (p - c).length();
                    if d >= CRATER_REACH * r {
                        continue;
                    }
                    let fresh = unit(h5).powf(FRESH_POWER * 2.5 / (1.0 + 0.5 * k as f32));
                    // On a mare only the craters younger than it stand; the older ones lie under the lava.
                    let stands = if fresh > POST_MARE { 1.0 } else { 1.0 - mare };
                    if stands <= 0.0 {
                        continue;
                    }
                    albedo *= crater(p, c, d, r, fresh, footprint, stands, slope);
                }
            }
        }
    }
    albedo
}

/// The ray craters at `p`: how much ray and halo material lies there (0..about 1.5), and the dark collar's darkening.
/// - The crater bright inside; its continuous ejecta a bright nimbus out to about three radii, and the diffuse light
///   of its ray zone fading out to about ten.
/// - Its rays: many thin streaks, one in each sector of azimuth round it, most faint and a few bright, of every length
///   up to its reach, fading along it as their ejecta thin out (a long ray of Tycho's is faint where it crosses
///   Nubium), beaded with gaps (chains of secondary craters). Where they crowd near the crater their material
///   saturates rather than adding up.
fn rays(prep: &Prepared, p: V3, footprint: f32) -> (f32, f32) {
    let footprint_km = footprint * R_MOON_KM;
    let (mut amount, mut collar) = (0.0f32, 0.0f32);
    for rc in &prep.rays {
        let cos_d = p.dot(rc.c);
        if cos_d < rc.cos_reach {
            continue;
        }
        let rho = cos_d.clamp(-1.0, 1.0).acos() * R_MOON_KM;
        let rr = rc.radius_km;
        let spread = rr.max(0.7 * footprint_km);
        let dim = (rr / spread) * (rr / spread);
        let t = rho / spread;
        let inside = 1.0 - smoothstep(0.8, 1.1, t);
        let nimbus = (-((t - 1.0).max(0.0) / 1.2).powi(2)).exp();
        let zone = (-(t - 1.0).max(0.0) / 4.0).exp();
        amount += rc.strength * dim * (0.7 * inside + 0.4 * nimbus) + rc.strength * 0.25 * zone;
        if rc.collar > 0.0 {
            collar += rc.collar * dim * (-((t - 1.4) / 0.3).powi(2)).exp();
        }
        let tangent = p - rc.c * cos_d;
        let az = tangent.dot(rc.north).atan2(tangent.dot(rc.east));
        let sectors = rc.rays as f32;
        let at = (az / TAU).rem_euclid(1.0) * sectors;
        let s0 = at.floor() as i32;
        let beads_keep = keep(35.0, footprint_km);
        let mut sum = 0.0f32;
        for ds in -2..=2 {
            let s = (s0 + ds).rem_euclid(rc.rays as i32) as u32;
            let h = hash_u32(rc.seed ^ s.wrapping_mul(0x85eb_ca6b));
            let (u1, u2, u3, u4) = (unit(h), unit(hash_u32(h ^ 0x1)), unit(hash_u32(h ^ 0x2)), unit(hash_u32(h ^ 0x3)));
            let ray_az = (s as f32 + 0.1 + 0.8 * u1) / sectors * TAU;
            let mut da = az - ray_az;
            da -= TAU * (da / TAU).round();
            if da.abs() > 0.5 * PI {
                continue;
            }
            let len = rc.reach_km * (0.15 + 0.85 * u2 * u2);
            if rho > len {
                continue;
            }
            let bright = 0.25 + 0.45 * u4 * u4;
            let width = (2.0 + 5.0 * u3) * (1.0 + rho / 700.0);
            let w = width.max(footprint_km);
            let across = rho * da.sin();
            let shape = (-(across / w).powi(2)).exp() * (width / w);
            let along = smoothstep(1.2 * rr, 2.5 * rr, rho) * (1.0 - smoothstep(0.0, len, rho));
            let n = 0.5 + 0.5 * perlin3(rho / 35.0, s as f32 * 7.31, u4 * 13.0, rc.seed);
            let beads = smoothstep(0.25, 0.75, n) * beads_keep + 0.5 * (1.0 - beads_keep);
            sum += bright * shape * along * beads;
        }
        amount += rc.strength * (1.0 - (-1.5 * sum).exp());
    }
    (amount, collar)
}

/// The surface at the unit point `p`, filtered over `footprint` (an arc of the unit sphere).
fn surface(prep: &Prepared, p: V3, footprint: f32) -> Surface {
    let (mare, mare_tone, mare_color, edge_km) = maria(prep, p, footprint);
    let land = highlands(p, edge_km, footprint);
    let mut albedo = land + (mare_tone - land) * mare;
    let color = mare_color * mare;
    let mut slope = V3::default();
    albedo *= craters(p, footprint, mare, &mut slope);
    albedo *= named_craters(prep, p, footprint, &mut slope);
    let (ray, collar) = rays(prep, p, footprint);
    albedo += (RAY_ALBEDO - albedo).max(0.0) * (0.7 * ray).min(0.85);
    albedo *= 1.0 - collar.min(0.6);
    Surface { albedo, color, slope }
}

/// The local east and north on the sphere at the unit point (x, y, z) of the body frame (x east, y north, z toward the
/// Earth): east along the latitude circle, north along the meridian.
fn tangents(x: f32, y: f32, z: f32) -> (V3, V3) {
    let c = (x * x + z * z).sqrt().max(1.0e-4);
    (V3::new(z / c, 0.0, -x / c), V3::new(-x * y / c, c, -y * z / c))
}

/// The normal the relief bends the sphere's to at the disc point (x east, y north) whose sphere point faces the Earth
/// by `cos_e`, for the slopes along the local east and north (`MoonFace::sample`'s third and fourth): the unit normal
/// in the body frame (x east, y north, z toward the Earth).
pub fn relief_normal(x: f32, y: f32, cos_e: f32, slope_east: f32, slope_north: f32) -> [f32; 3] {
    let (e, n) = tangents(x, y, cos_e);
    let v = V3::new(x, y, cos_e) - e * slope_east - n * slope_north;
    let v = v.normalize();
    [v.x, v.y, v.z]
}

/// A sample's albedo per channel: its albedo plus its colour index along COLOR_AXIS.
pub fn albedo_rgb(s: [f32; 4]) -> [f32; 3] {
    [s[0] + s[1] * COLOR_AXIS[0], s[0] + s[1] * COLOR_AXIS[1], s[0] + s[1] * COLOR_AXIS[2]]
}

/// The moon's face as a pyramid over the disc the Earth sees (see the module notes). A texel holds the albedo (its
/// mean over the disc 1), the albedo times its colour index (its mean over the disc 0), and the slopes along the
/// local east and north.
pub struct MoonFace {
    levels: Vec<Vec<[f32; 4]>>,
}

impl std::fmt::Debug for MoonFace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MoonFace").field("size", &FACE_SIZE).field("levels", &self.levels.len()).finish()
    }
}

impl MoonFace {
    /// Works the face out, its finest level's rows over `run`; None when the runner skipped a row (an overtaken job's).
    pub fn build(run: &dyn Fn(usize, &(dyn Fn(usize) + Sync))) -> Option<MoonFace> {
        let prep = Prepared::new();
        let n = FACE_SIZE;
        let texel = 2.0 / n as f32;
        let rows: Vec<OnceLock<Vec<[f32; 4]>>> = (0..n).map(|_| OnceLock::new()).collect();
        run(n, &|j| {
            let y = 1.0 - (j as f32 + 0.5) * texel;
            let row = (0..n)
                .map(|i| {
                    let x = (i as f32 + 0.5) * texel - 1.0;
                    let r = (x * x + y * y).sqrt();
                    let (x, y) = if r > LIMB_REACH { (x * LIMB_REACH / r, y * LIMB_REACH / r) } else { (x, y) };
                    let z = (1.0 - x * x - y * y).max(0.0).sqrt();
                    // The texel's arc on the sphere: its width across the disc, stretched by 1 / z toward the limb
                    // along the radius (the geometric mean of the two).
                    let footprint = texel / z.max(0.05).sqrt();
                    let s = surface(&prep, V3::new(x, y, z), footprint);
                    let (e, nn) = tangents(x, y, z);
                    [s.albedo, s.albedo * s.color, s.slope.dot(e), s.slope.dot(nn)]
                })
                .collect();
            let _ = rows[j].set(row);
        });
        let mut level0 = Vec::with_capacity(n * n);
        for row in rows {
            level0.extend(row.into_inner()?);
        }
        // The albedo's mean over the disc to 1 and its colour's to 0 (the moon's colour is MOON_TINT's).
        let (mut sum_a, mut sum_c, mut count) = (0.0f64, 0.0f64, 0usize);
        for j in 0..n {
            let y = 1.0 - (j as f32 + 0.5) * texel;
            for i in 0..n {
                let x = (i as f32 + 0.5) * texel - 1.0;
                if x * x + y * y < 1.0 {
                    let t = level0[j * n + i];
                    sum_a += t[0] as f64;
                    sum_c += t[1] as f64;
                    count += 1;
                }
            }
        }
        let mean_a = (sum_a / count.max(1) as f64) as f32;
        let mean_c = (sum_c / sum_a.max(1.0e-9)) as f32;
        for t in &mut level0 {
            let a = t[0] / mean_a;
            let c = t[1] / t[0].max(1.0e-6) - mean_c;
            *t = [a, a * c, t[2], t[3]];
        }
        let mut levels = vec![level0];
        for k in 1..FACE_LEVELS {
            let (src, m) = (&levels[k - 1], FACE_SIZE >> (k - 1));
            let half = m / 2;
            let mut next = Vec::with_capacity(half * half);
            for j in 0..half {
                for i in 0..half {
                    let at = |di: usize, dj: usize| src[(2 * j + dj) * m + 2 * i + di];
                    let (a, b, c, d) = (at(0, 0), at(1, 0), at(0, 1), at(1, 1));
                    next.push(std::array::from_fn(|ch| 0.25 * (a[ch] + b[ch] + c[ch] + d[ch])));
                }
            }
            levels.push(next);
        }
        Some(MoonFace { levels })
    }

    /// The face at the disc point (x east, y north; the disc's radius is 1), filtered over `footprint` (how far apart
    /// the lookups are, in the same units): trilinear over the pyramid. Its albedo, albedo times colour index, and
    /// slopes along the local east and north.
    pub fn sample(&self, x: f32, y: f32, footprint: f32) -> [f32; 4] {
        let lod = (footprint * (0.5 * FACE_SIZE as f32)).max(1.0e-6).log2().clamp(0.0, (FACE_LEVELS - 1) as f32);
        let l0 = (lod as usize).min(FACE_LEVELS - 1);
        let t = lod - l0 as f32;
        let a = self.bilinear(l0, x, y);
        if t > 0.0 && l0 + 1 < FACE_LEVELS {
            let b = self.bilinear(l0 + 1, x, y);
            std::array::from_fn(|ch| a[ch] + (b[ch] - a[ch]) * t)
        } else {
            a
        }
    }

    fn bilinear(&self, level: usize, x: f32, y: f32) -> [f32; 4] {
        let n = FACE_SIZE >> level;
        let data = &self.levels[level];
        let u = ((x + 1.0) * 0.5 * n as f32 - 0.5).clamp(0.0, (n - 1) as f32);
        let v = ((1.0 - y) * 0.5 * n as f32 - 0.5).clamp(0.0, (n - 1) as f32);
        let (i0, j0) = (u as usize, v as usize);
        let (i1, j1) = ((i0 + 1).min(n - 1), (j0 + 1).min(n - 1));
        let (fu, fv) = (u - i0 as f32, v - j0 as f32);
        let (a, b, c, d) = (data[j0 * n + i0], data[j0 * n + i1], data[j1 * n + i0], data[j1 * n + i1]);
        std::array::from_fn(|ch| (a[ch] + (b[ch] - a[ch]) * fu) + ((c[ch] + (d[ch] - c[ch]) * fu) - (a[ch] + (b[ch] - a[ch]) * fu)) * fv)
    }

    /// The pyramid packed for a GPU, ATLAS_WIDTH x ATLAS_HEIGHT RGBA texels row by row from the top (north): level 0
    /// at the left, level k (k >= 1) at x FACE_SIZE, y FACE_SIZE - 2 (FACE_SIZE >> k); the rest 0.
    pub fn atlas(&self) -> Vec<f32> {
        let mut out = vec![0.0f32; ATLAS_WIDTH * ATLAS_HEIGHT * 4];
        for (k, level) in self.levels.iter().enumerate() {
            let n = FACE_SIZE >> k;
            let (x0, y0) = if k == 0 { (0, 0) } else { (FACE_SIZE, FACE_SIZE - 2 * n) };
            for j in 0..n {
                for i in 0..n {
                    let at = ((y0 + j) * ATLAS_WIDTH + x0 + i) * 4;
                    out[at..at + 4].copy_from_slice(&level[j * n + i]);
                }
            }
        }
        out
    }
}

static FACE: OnceLock<Arc<MoonFace>> = OnceLock::new();

/// The process's face, built over `run` the first time (about 1 s of work: a tenth of that on 16 cores); None when the
/// runner skipped a row then.
pub fn face(run: &dyn Fn(usize, &(dyn Fn(usize) + Sync))) -> Option<Arc<MoonFace>> {
    if let Some(face) = FACE.get() {
        return Some(face.clone());
    }
    let built = Arc::new(MoonFace::build(run)?);
    Some(FACE.get_or_init(|| built).clone())
}

/// The process's face if a bake has built it.
pub fn built_face() -> Option<Arc<MoonFace>> {
    FACE.get().cloned()
}
