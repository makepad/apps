//! Night layers of the Sky mode: stars, the moon and the night glow.
//!
//! **Brightness.** Everything is scaled against the atmosphere's `SUN_IRRADIANCE`,
//! so the ratios inside the night are physical:
//! - a full moon is about 1/400000 of the sun, and its disc about as bright as a
//!   daytime sky;
//! - a dark moonless sky (glow_strength 1) is about 22 mag/arcsec²;
//! - a star's irradiance follows its magnitude.
//!
//! On top of that the night is *adapted*. As the sun sinks from the horizon to
//! −12°, the night layers are exposed up by NIGHT_ADAPTATION_EV, the way eyes and
//! night photographs adapt, and so is everything the sun lights ([`adaptation`]:
//! the sky and its ground, the clouds, the floors, the key), so the night keeps its
//! real ratio to the twilight: at −12° the natural glow overhead is about a tenth
//! of the twilight (a 22 mag/arcsec² sky against the twilight Koomen et al. 1952
//! measured: a fifth to a seventh), where the night lifted alone made it 90 times.
//! Three things follow:
//! - the daytime moon stays physical, faint against a blue sky;
//! - twilight hands over to night without a jump;
//! - a night map exports at levels a renderer can use.
//!
//! **Stars** sit on the celestial sphere and wheel with the solar hour around the
//! pole for the latitude (`crate::sun::celestial_rows`). They hang on the 3D grid
//! of the sky shader (`shaders/sky_dome.rs:171-193`), evaluated on the CPU:
//! - space is cut into cubes; each cube hashes one jittered point and holds a
//!   star where that point, normalised, meets the unit sphere. A cube lattice cut
//!   by the sphere gives a uniform field with no pinch at the poles;
//! - a star is a Gaussian of the perpendicular distance to it, of a fixed angular
//!   size: a sharp core plus a halo, so it neither aliases away at 512×256 nor
//!   lands on a single pixel at 8K;
//! - brightness follows a magnitude-like distribution, colour a B-V index drawn
//!   as the naked-eye sky's are distributed (Ballesteros 2012 takes it to a
//!   temperature), and the field is thicker along the Milky Way.
//!
//! **Extinction.** Everything from beyond the air (stars, the moon, the airglow's
//! shells, the Milky Way, the nebulas, the aurora) is dimmed and reddened by the
//! scene's own atmosphere: its transmittance to space (`Atmosphere::transmittance`),
//! haze, air and ozone included, tabulated over the elevation once per map.
//!
//! **Moon.** A sphere lit from the sun's direction, so its phase follows the
//! sun-moon geometry.
//! - The terminator is where the surface turns away from the sun (n·sun = 0).
//! - The lit side's light spreads as Hapke's law has it for the Moon's rough
//!   regolith (Helfenstein & Veverka 1987), drawn as its lunar-Lambert fit: a full
//!   moon is flat bright out to the limb (the dust scatters back; a Lambert ball
//!   would darken), and as the phase grows the light gathers toward the middle of
//!   the bright limb and the horns fade in the rough surface's shadows.
//! - Its whole light follows the measured lunar phase law (Allen: m = -12.73 +
//!   0.026|a| + 4e-9 a^4, the opposition surge included), so a quarter moon is
//!   about a tenth of a full one.
//! - Earthshine, the ashen light, lights the night side: the sunlit Earth at its
//!   phase seen from the moon and its measured albedo (Goode et al. 2001), sent
//!   back as a full moon is, and bluer (Thejll et al. 2014). About 11 stops under
//!   a crescent's brightest part, none at full.
//! - Its light scatters in the air through the same `Atmosphere` the sun's day sky
//!   comes from, with the moon as its sun: the blue sky, the aureole, the haze and
//!   the stratosphere, the multiple scattering and the ground's bounce all follow.
//! - Its face is the near side, generated (`moon_face`): the maria where the real ones
//!   are, the bright highlands and ray craters, and the relief, which bends the normal
//!   the light law reads, so it shows along the terminator and never on the night
//!   side. The earthshine lights the night side through the same albedo. The face is
//!   scaled per phase so the disc's whole light is what Allen's law gives, as before.
//! - Its north: the Moon's equator holds the sun (to 1.5 degrees) and faces the Earth
//!   (to its libration), so its axis is square to the plane of the sun, the Earth and
//!   the moon, on the celestial north's side; near full or new moon, where that plane
//!   is lost, the celestial pole (`moon_frame`). The terminator then runs from the
//!   moon's north to its south at every placing, as on the real moon, and a moon east
//!   of the sun (waxing) shows its eastern maria (Crisium) first.
//!
//! **Glow** is the natural night sky's own light, 22 mag/arcsec² at the zenith at
//! strength 1:
//! - airglow's lines, each a thin shell: OI 557.7 nm (green, 97 km), Na 589.3 nm
//!   (92 km) and OI 630.0 nm (red, 250 km), at a quiet night's zenith rayleighs;
//! - the continuum (airglow's own, the zodiacal light, the faint stars), a third
//!   in a shell at 90 km and the rest even over the sky;
//! - the shells brighten toward the horizon by van Rhijn's factor and the air
//!   dims them over the shell's own air mass (Krisciunas & Schaefer 1991: the
//!   light the air scatters back in makes up the rest), so with a clear sky the
//!   glow near the horizon is about 1.2 to 1.3 times the zenith's (dark-sky
//!   photographs: 1.1 to 1.2) and a little warm; haze dims it there.
//!
//! **A town's glow** (light pollution): the light a town sends up, scattered once
//! by the air and the haze toward the viewer (Garstang 1986), tabulated over the
//! elevation and the azimuth from the town once per map.

use makepad_draw::*;
use super::atmosphere::{Atmosphere, SUN_IRRADIANCE};
use super::aurora::{Aurora, AURORA_BARE_TEXEL};
use super::moon_face::{self, MoonFace};
use super::milky_way::MilkyWay;
use super::nebulas::Nebulas;
use super::noise::{hash01, hash_u32};
use super::spectrum;
use super::studio::{kelvin_to_rgb, light_frame, project, LightFrame};
use super::{dome_look, AtmosphereParams, EnvSun, LightPollutionParams, NebulaParams, NightParams, SunDiscParams, SunMode, SunParams, DOME_COLUMNS, DOME_ROWS};
use crate::sky::luminance;
use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU};
use std::sync::Arc;

/// Clamps into [lo, hi]. A NaN becomes `lo`.
fn sane(x: f32, lo: f32, hi: f32) -> f32 {
    x.max(lo).min(hi)
}

/// The local solar hour that turns the star dome (`celestial_rows`).
///
/// **Time mode.** The clock is corrected to the longitude:
/// solar = clock + longitude/15 − tz_offset. The equation of time is ignored, and
/// the stars follow the solar hour, not sidereal time (a known limitation).
///
/// **Manual mode.** There is no clock, so one is read off the sun's azimuth: east
/// is 6 h, the noon side 12 h, west 18 h. The stars therefore turn as the sun is
/// dragged.
pub fn star_hours(sun: &SunParams) -> f32 {
    let hours = match sun.mode() {
        SunMode::Time => sun.hour + sun.longitude / 15.0 - sun.tz_offset,
        SunMode::Manual => {
            let az = sun.azimuth_deg;
            if sun.latitude >= 0.0 {
                12.0 + (az - 180.0) / 15.0
            } else {
                // South of the equator the noon sun stands north, and the day runs
                // from east through north to west.
                let from_north = (az + 180.0).rem_euclid(360.0) - 180.0;
                12.0 - from_north / 15.0
            }
        }
    };
    if hours.is_finite() { hours.rem_euclid(24.0) } else { 12.0 }
}

/// EV the night layers are lifted by once the sun is 12 degrees down (see the module notes).
const NIGHT_ADAPTATION_EV: f32 = 10.0;

/// The night's adaptation for the sun along `sun_dir` (the map's frame): 1 with the sun up, rising smoothly as it
/// sinks (NIGHT_ADAPTATION_EV x smoothstep(0, 12, depression) in EV) to 2^NIGHT_ADAPTATION_EV once it is 12 degrees
/// down. The night layers are lifted by it ([`NightSky::new_par`]), and so is everything the sun lights
/// (`Atmosphere::lifted`, which the Env's sky is built with), so twilight keeps its real ratio to the night.
pub fn adaptation(sun_dir: Vec3f) -> f32 {
    let sun_elevation_deg = sun_dir.y.clamp(-1.0, 1.0).asin().to_degrees();
    2.0f32.powf(NIGHT_ADAPTATION_EV * smoothstep(0.0, 12.0, -sun_elevation_deg))
}

// ---- Stars ----

/// Cells per unit along each axis of the 3D grid the stars hang on. At the unit
/// sphere a cell is 1/STAR_GRID rad, about 2.1 degrees, across. About
/// 4π·STAR_GRID²·(2·STAR_SHELL) = 9,200 cells reach the sphere.
const STAR_GRID: f32 = 27.0;
/// A cell holds a star only when its jittered point lies within this many cells
/// of the unit sphere. Together with the reach it keeps every star inside the
/// 3x3x3 gather of the directions that see it: STAR_GRID x reach + STAR_SHELL < 1.
const STAR_SHELL: f32 = 0.5;
/// Share of the shell's cells that hold a star at `stars` = 1 (about 8,200 stars
/// over the sphere before the Milky Way boost; 4,100 at the default 0.5, 4,700
/// with the boost, as measured). The naked-eye sky holds about 9,000 stars to
/// magnitude 6.5.
const STAR_KEEP_MAX: f32 = 0.9;
/// The field is denser along the Milky Way: the keep share is multiplied by
/// 1 + MILKY_WAY_BOOST at the galactic equator, falling off as a Gaussian in the
/// sine of the galactic latitude with MILKY_WAY_SIN_HALF (sin 10 degrees) as its width.
const MILKY_WAY_BOOST: f32 = 1.0;
const MILKY_WAY_SIN_HALF: f32 = 0.17;
/// Faintest magnitude drawn.
///
/// Star counts grow about 3.2x per magnitude (10^0.5). A cell's hash h in (0, 1]
/// therefore maps to the magnitude m = LIMIT + 2·log10(h), and its flux relative to
/// the faintest star is h^-0.8.
pub(super) const STAR_MAG_LIMIT: f32 = 6.5;
/// h is floored so the brightest star is magnitude -1.5 (Sirius is -1.46).
const STAR_HASH_FLOOR: f32 = 1.0e-4;
/// The sun's apparent magnitude. A magnitude 0 star gives SUN_IRRADIANCE x 10^(0.4 x -26.74).
const SUN_MAGNITUDE: f32 = -26.74;
/// The Milky Way and the nebulas show from astronomical twilight on (plan hdri-7 §2: "visible only in astronomical
/// twilight and darker"): smoothstep(12, 18, -sun elevation). The stars are full from -12 already, and the lifted
/// night would otherwise show the band at full contrast in nautical twilight.
pub(super) const ASTRO_FADE_DEG: (f32, f32) = (12.0, 18.0);
/// Square arcseconds in a steradian: (180 / pi x 3600)^2.
const ARCSEC2_PER_SR: f32 = 4.254_517e10;
/// The SQM conversion: a sky of 0 mag/arcsec² shines 10.8e4 cd/m².
const SQM_CD_M2: f32 = 10.8e4;

/// A surface brightness `mu` (mag/arcsec²) as unadapted engine luminance, on the stars' own anchor (a magnitude-mu
/// star in every square arcsecond): 17.13 x 10^(-0.4 mu). Plan hdri-7 (the Milky Way, the nebulas).
pub(super) fn mag_arcsec2_to_radiance(mu: f32) -> f32 {
    SUN_IRRADIANCE * 10.0f32.powf(0.4 * (SUN_MAGNITUDE - mu)) * ARCSEC2_PER_SR
}

/// cd/m² per unadapted engine unit, from the SQM conversion on the same anchor: about 6303 (the sun's 20 units of
/// irradiance are then about 126 klx). Plan hdri-7 (the aurora's lines, `spectrum::line_xyz_per_rayleigh`).
pub(super) fn cd_m2_per_engine_unit() -> f32 {
    SQM_CD_M2 / mag_arcsec2_to_radiance(0.0)
}
/// A star is two parts, both of a fixed angular size:
/// - a sharp core, about a pixel at 4K;
/// - a halo about half a pixel wide at 512x256, which keeps the brighter stars
///   visible in the preview.
const STAR_SIGMA_CORE: f32 = 0.05 * PI / 180.0;
const STAR_SIGMA_HALO: f32 = 0.35 * PI / 180.0;
const STAR_CORE_SHARE: f32 = 0.6;
/// Squared reach of a star: 3 halo sigmas (1.05 degrees), as the sine of the angle.
pub(super) const STAR_REACH2: f32 = 9.0 * STAR_SIGMA_HALO * STAR_SIGMA_HALO;
/// The Gaussians are cut at 3 sigma and lowered by their value there, so they reach
/// zero smoothly. These are e^-4.5, and the mass left relative to 2 pi sigma^2,
/// which is 1 - 5.5·e^-4.5.
const GAUSS_FLOOR: f32 = 0.011_108_997;
const GAUSS_MASS: f32 = 0.938_900_5;
const STAR_COLORS: usize = 16;
/// Stars look paler than their blackbody colour; this share of it is kept.
const STAR_SATURATION: f32 = 0.6;

// ---- Moon ----

/// The Moon's albedo.
/// - A full moon's disc is ALBEDO x SUN_IRRADIANCE / pi, about 0.76 before extinction.
/// - Its illuminance is about 1/400000 of the sun's.
const MOON_ALBEDO: f32 = 0.12;
/// Moonlight is a little redder than sunlight.
const MOON_TINT: Vec3f = vec3f(1.05, 1.0, 0.92);
/// Earthshine (the ashen light): the sunlit Earth lights the moon's night side (`earthshine`).
/// - The Earth is a Lambert sphere of albedo EARTH_ALBEDO: the earthshine photometry of Big Bear measures the Earth's
///   albedo at 0.297 +- 0.005 (Goode et al. 2001, GRL 28, 1671), as the apparent albedo p* of Qiu et al. 2003 (JGR 108,
///   4709) and Pallé et al. 2003 (JGR 108, 4710): the albedo of the Lambert sphere that reflects as the real Earth does at
///   that phase. The weather moves it by a few percent from night to night; it is held here.
/// - Seen from the moon the Earth shows at the supplement of the moon's phase angle (full at new moon), EARTH_RADIUS_KM
///   across MOON_DISTANCE_KM (the mean distance): its light on the moon is p* x 2/3 Phi x (R / d)^2 of the sunlight, Phi
///   the Lambert sphere's phase function, at most 5.5e-5 (a full Earth).
/// - It lights the moon from almost straight behind us, so the moon sends it back at under a degree of phase
///   (EARTHSHINE_PHASE_DEG; the Earth is 1.9 degrees across from the moon): as a full moon does, even over the disc, the
///   opposition surge included.
/// Against a full moon's disc the ashen light is then 5.4e-5 at new moon, 4.3e-5 on a crescent at 140 degrees of phase
/// (about 11 stops under the crescent's brightest part: photographs show it with the crescent blown out), 1.7e-5 at a
/// quarter and none at full.
const EARTH_ALBEDO: f32 = 0.30;
const MOON_DISTANCE_KM: f32 = 384_400.0;
const EARTHSHINE_PHASE_DEG: f32 = 1.0;
/// Earthlight is bluer than sunlight (the sunlit Earth's Rayleigh sky): the earthlit side of the moon measures 0.150 +-
/// 0.005 mag bluer in B-V than its sunlit side (Thejll et al. 2014, A&A 563, A38). As a smooth spectral slope through B
/// (445 nm) and V (551 nm), lambda^-0.65, at the channels' 610, 550 and 465 nm: 0.94, 1, 1.12 over the moonlight's colour,
/// its luminance kept.
const EARTHSHINE_TINT: Vec3f = vec3f(0.94, 1.0, 1.12);
/// How the moon's light spreads over its face: Hapke's law (Hapke 1981, JGR 86, 3039; 1984, Icarus 59, 41) with the
/// Moon's disk-integrated parameters of Helfenstein & Veverka (1987, Icarus 72, 342): single-scattering albedo 0.21, a
/// Henyey-Greenstein lobe of asymmetry -0.18 (backward), the shadow-hiding opposition surge of amplitude 2.01 and width
/// 0.07, and a mean slope of 20 degrees (the Moon's macroscopic roughness, Hapke 1984).
/// - The surge is the same over the whole face (the phase angle is, to a quarter of a degree), so it shapes only the whole
///   light (Allen's law, which the face is scaled to) and the earthshine.
/// - The roughness and the multiple scattering shape the face: a full moon stays flat out to the limb, and as the phase
///   grows the light gathers toward the middle of the bright limb and the horns fade, where Lommel-Seeliger kept a
///   rim twice the middle's out to the cusps.
const HAPKE_W: f32 = 0.21;
const HAPKE_XI: f32 = -0.18;
const HAPKE_B0: f32 = 2.01;
const HAPKE_H: f32 = 0.07;
const HAPKE_SLOPE_DEG: f32 = 20.0;
/// The face is drawn as the lunar-Lambert mix (McEwen 1991, Icarus 92, 298), L x Lommel-Seeliger + (1 - L) x Lambert,
/// with L fitted to the Hapke face by least squares over a FACE_FIT_N x FACE_FIT_N grid of the disc once per map
/// (`lunar_lambert_share`): within 3 to 9 % RMS of it from 0 to 160 degrees of phase, cheap per texel and the same in
/// the app's preview. L is 1 at full moon, 0.64 at 30 degrees, 0.20 at a quarter and 0 past 120 degrees: within 0.06 of
/// McEwen's lunar L(g) = 1 - 0.019 g + 2.42e-4 g^2 - 1.46e-6 g^3 up to a quarter, where his polynomial holds.
const FACE_FIT_N: usize = 40;
/// Width of the soft limb, as a fraction of the radius.
const MOON_LIMB: f32 = 0.06;
/// The moon's whole light falls with the phase angle a as Allen's lunar phase law (Astrophysical Quantities):
/// m(a) - m(0) = 0.026 |a| + 4e-9 a^4 (a in degrees). Past this angle (a crescent two days from new) the law is
/// not measured, and the scale it gives over the face's own fall is held.
const MOON_PHASE_MAX_DEG: f32 = 165.0;
/// The moonlit sky's atmosphere is built only while the moon is no lower than this (sine of the elevation, about
/// -6 degrees): lower, its twilight in the air is far below the night glow.
const MOON_AIR_MIN_SIN: f32 = -0.1;
/// What share of a lookup's texel the moon's face is filtered over (`NightSky::moon_at`). A bake refines the texels of
/// a disc under HOT_MAX_TEXELS texels in radius as the mean of 8 x 8 lookups over each (`refine_hot_spots`): half a
/// texel each, and the mean does the rest. A larger disc is read once at each texel's centre, so over the whole texel:
/// read over half, the face's detail between the map's Nyquist limit and twice it folded back (a 10 degree quarter
/// moon in an 8K map moved a quarter texel changed by 14 to 20 % RMS against the same image moved without aliasing,
/// 5 to 7 % read over the whole texel, as a fine box filter does).
const MOON_FOOTPRINT_REFINED: f32 = 0.5;
const MOON_FOOTPRINT: f32 = 1.0;
/// How firmly the celestial pole holds the moon's north while the plane of the sun, the Earth and the moon narrows
/// toward full or new moon (`moon_frame`): the pole and the plane weigh the same about 8 degrees from full.
const MOON_POLE_HOLD: f32 = 0.02;

// ---- Glow ----

/// Zenith glow radiance before adaptation at the default glow_color and strength 1 is luminance([0.02, 0.03, 0.06])
/// x GLOW_SCALE (G0, 3.0e-8 at SUN_IRRADIANCE 20): a dark rural sky, about 22 mag/arcsec²; strength 10 is a
/// suburban one. Tied to the sun's scale like every other night level.
const GLOW_SCALE: f32 = 5.0e-8 * SUN_IRRADIANCE;
/// The default glow colour: at it the glow is the physical one (the lines' own colours and CONTINUUM_TINT), and the
/// glow colour tints the whole glow, the lines and the continuum alike, channel by channel against it: black turns it
/// off.
const DEFAULT_GLOW_COLOR: [f32; 3] = [0.02, 0.03, 0.06];
/// The night sky's emission lines: wavelength (nm), zenith intensity on a quiet night at solar mean (rayleighs) and
/// the emitting shell's height (km). OI 557.7 nm 100-250 R over the solar cycle at 95-100 km; Na D 30-150 R
/// (strongest in winter) at about 92 km; OI 630.0 nm 50-100 R in a broad layer near 250 km (Roach & Gordon, The
/// Light of the Night Sky). The green line is taken at a very quiet night's 60 R, measured against Poly Haven's
/// Rogland clear night (the r, g, b shares of its sky at 10, 30 and 70 degrees): its shell gains on the even
/// continuum toward the horizon, almost all of it in green, and at 120 R the sky's green share ran 0.025 over the
/// photograph's at 10 degrees and 0.019 at 30 (olive); at 60 R every share is within 0.015.
const AIRGLOW_LINES: [(f32, f32, f32); 3] = [(557.7, 60.0, 97.0), (589.3, 50.0, 92.0), (630.0, 60.0, 250.0)];
/// The continuum's colour (luminance 1 once normalised): airglow's own, the zodiacal light and the faint stars
/// together, fitted with the green line's strength to the Rogland photograph's shares at 10, 30 and 70 degrees
/// (r, g, b: 0.380, 0.339, 0.281; 0.357, 0.329, 0.314; 0.325, 0.332, 0.343).
const CONTINUUM_TINT: Vec3f = vec3f(0.85, 1.0, 1.27);
/// The continuum's shell height (km), and the share of it that comes from beyond the air and lies evenly over the
/// sky instead: of a dark site's V-band sky the lines are about 30 %, airglow's own continuum about 25 % and the
/// zodiacal light and the faint stars about 45 % (Leinert et al. 1998), so about 0.65 of the continuum.
const CONTINUUM_KM: f32 = 90.0;
const CONTINUUM_EVEN: f32 = 0.65;
/// The share of the direct light's reddening the natural glow keeps toward the horizon (`Airglow::exact`): about half
/// of the light there is scattered back in, and bluer. At 1 the sky from 15 to 40 degrees went khaki and at the
/// horizon orange where photographs are grey.
const GLOW_REDDENING: f32 = 0.5;
const EARTH_RADIUS_KM: f32 = 6371.0;
/// Rows of the extinction table, over sqrt(sin elevation) from the horizon to the zenith.
const EXTINCTION_ROWS: usize = 48;
/// Rows of the natural glow's table (`Airglow::seen`), over sqrt(sin elevation) from the horizon to the zenith as the
/// extinction's. Read linearly it is within 0.04 % of `Airglow::exact` over the whole range of the air (the worst with
/// the haze, the air and the ozone at 10 and the stratosphere at 20, 9 degrees up; 4e-6 in the default air), where
/// 256 rows were 0.15 % off.
const GLOW_ROWS: usize = 512;

// ---- A town's glow ----

/// The town's dome is tabulated over the elevation (squared scale, rows) and the azimuth away from the town
/// (squared scale, columns), single scattering along each view ray in DOME_STEPS steps out to where the ray leaves the
/// scattering air, fine where it passes the town and growing away from there (`town_scatter`). Over the dome's looks
/// that is within 1.3 % of a march of 4096 steps at every haze, town size and distance (0.7 % for a town of 100 000 in
/// clear air), where 64 steps out to 300 km were up to 11 % off within 30 km of the town and about 60 % at 300 km.
const DOME_ROWS_N: usize = 32;
const DOME_COLUMNS_N: usize = 48;
const DOME_STEPS: usize = 96;
/// Light is scattered only below this height (the air above holds a thousandth of it).
const DOME_TOP_M: f32 = 40_000.0;
/// The town's lit area: a disc of this radius at Strength 1 (a town of 100 000), growing as the square root of the
/// people (the same density), held to TOWN_RADIUS_RANGE_M. Its light leaves from the centre and six points round it
/// at TOWN_RING of the radius, each soft over TOWN_SOFT of the radius, so a view along the ground over it stays finite.
const TOWN_RADIUS_M: f32 = 3_000.0;
const TOWN_RADIUS_RANGE_M: (f32, f32) = (1_000.0, 20_000.0);
const TOWN_RING: f32 = 0.6;
const TOWN_SOFT: f32 = 0.5;
/// The houses and trees round the lamps take the light they send within a few degrees of level (Garstang's F term
/// peaks there): what leaves under this elevation sine (about 6 degrees) fades out toward level.
const TOWN_OBSTRUCTION_SIN: f32 = 0.1;
/// The lamps' colours as they look (their spectra's Rec.709 renditions are far more saturated than photographs of a
/// town's dome show): high-pressure sodium at about 2100 K, white LED at 4000 K.
const SODIUM_KELVIN: f32 = 2100.0;
const LED_KELVIN: f32 = 4000.0;
/// Garstang's (1986) upward light of a town: 2 G (1 - F) cos psi + 0.554 F psi^4 at the zenith angle psi, the ground's
/// reflectance G and the share F the lamps send straight up and sideways.
const TOWN_GROUND_REFLECTANCE: f32 = 0.15;
const TOWN_DIRECT_SHARE: f32 = 0.1;
/// Walker's law (1977), the calibration: a town of P people D km away lifts the sky 45 degrees up toward it by
/// 0.01 P D^-2.5 of the natural sky's zenith. The dome is scaled once so a town at TOWN_REF_KM does so through a
/// clear sky (haze 1, air 1); other distances, hazes and heights follow from the scattering.
const WALKER: f32 = 0.01;
const TOWN_REF_KM: f32 = 20.0;
const TOWN_PEOPLE_PER_STRENGTH: f32 = 100_000.0;
/// The air's Rayleigh scattering at sea level per metre at the channels' wavelengths (610, 550, 465 nm; 1.24e-6 /
/// lambda^4 in um), with its scale height; the haze's at 550 nm per unit of Haze (the atmosphere's vertical optical
/// depth 0.07 over its 1200 m scale height), its Angstrom exponent, single-scattering albedo and forward lobe.
const DOME_RAYLEIGH: Vec3f = vec3f(8.95e-6, 13.56e-6, 26.6e-6);
const DOME_RAYLEIGH_H: f32 = 8_000.0;
const DOME_MIE_550: f32 = 0.07 / 1_200.0;
const DOME_MIE_H: f32 = 1_200.0;
const DOME_MIE_ANGSTROM: Vec3f = vec3f(0.874, 1.0, 1.244);
const DOME_MIE_ALBEDO: f32 = 0.9;
const DOME_MIE_G: f32 = 0.7;
/// The viewer's height above the ground, as the atmosphere's.
const DOME_VIEWER_M: f32 = 200.0;

/// Stars, moon and glow for one sun position, prepared once per map.
#[derive(Clone, Debug)]
pub struct NightSky {
    /// 0 by day, 1 once the sun is 12 degrees down.
    night_factor: f32,
    sun_dir: Vec3f,
    sun_elevation_deg: f32,
    /// World to star-sphere rotation, as rows (`celestial_rows`). y is the celestial pole.
    rows: [Vec3f; 3],
    /// The galactic pole in star coordinates: the Milky Way runs where a star's
    /// dot with it is near zero.
    gal_pole: Vec3f,
    /// A shell cell holds a star when its density roll is below this (before the
    /// Milky Way boost) ...
    star_keep: f32,
    /// ... and never when it is at or above this (after the boost): the cheap
    /// first test that lets most empty cells cost one hash.
    star_keep_max: f32,
    /// Irradiance of a magnitude-LIMIT star, brightness and adaptation included.
    star_gain: f32,
    star_seed: u32,
    star_colors: [Vec3f; STAR_COLORS],
    /// The air's transmittance to space over sqrt(sin elevation), EXTINCTION_ROWS rows from the horizon up.
    extinction: [Vec3f; EXTINCTION_ROWS],
    /// The natural glow's lines and continuum, adaptation included.
    airglow: Airglow,
    /// The moonlit air: the moon's own atmosphere; None by day, with no moon or with the moon well down.
    moon_air: Option<Arc<MoonAir>>,
    /// A town's glow; None when its block does not draw.
    town: Option<TownDome>,
    moon: Option<Moon>,
    /// Mean upper-hemisphere radiance of the glow and the moonlit air.
    ambient: Vec3f,
    /// Irradiance on flat ground from the night sky and the moon.
    ground_irradiance: Vec3f,
    /// The moon's own part of it, by the night factor.
    moon_ground: Vec3f,
    /// The galactic centre (l 0), pole and l 90 in star space: constants of the star sphere (plan hdri-7 §2).
    gal_axes: [Vec3f; 3],
    /// The Milky Way; Some when its block draws (plan hdri-7 §2).
    milky_way: Option<MilkyWay>,
    /// The nebulas; Some when their block draws (plan hdri-7 §3).
    nebulas: Option<Nebulas>,
    /// The aurora; Some when its block draws and the sun is below the horizon (plan hdri-7 §4).
    aurora_layer: Option<Aurora>,
    /// The sine of the horizon's elevation, 0 or below: the stars and the moon are drawn down to it and the glow fades
    /// into the ground below it. Below level from a viewer height over the planet's ground (`Atmosphere::horizon_dip`),
    /// else 0.
    horizon_sin: f32,
}

/// One star of the field, in star space.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Star {
    /// Unit direction on the celestial sphere.
    dir: Vec3f,
    /// Irradiance, adaptation included.
    flux: f32,
    /// Luminance-1 colour.
    color: Vec3f,
}

/// The natural night glow (see the module notes), adaptation, strength and colour included.
#[derive(Clone, Debug)]
struct Airglow {
    /// Each line's zenith radiance above the air and its shell's van Rhijn constant (R / (R + h))².
    lines: [(Vec3f, f32); 3],
    /// The continuum's zenith radiance above the air.
    continuum: Vec3f,
    /// The air's transmittance at the zenith.
    zenith_t: Vec3f,
    /// `exact` over sqrt(sin elevation), GLOW_ROWS rows from the horizon to the zenith: what `seen` reads.
    table: [Vec3f; GLOW_ROWS],
}

impl Airglow {
    /// The glow seen from the ground at elevation sine `y` (the horizon's below it): `exact` as its table holds it,
    /// linear between rows. Every sky texel and every ground texel by night reads it (`glow`, `glow_at_horizon`), and
    /// `exact` costs sixteen powf.
    fn seen(&self, y: f32) -> Vec3f {
        sqrt_rows_at(&self.table, y)
    }

    /// The glow seen from the ground at elevation sine `y` (>= 0), after Krisciunas & Schaefer (1991, after
    /// Garstang): a shell seen through X = van Rhijn's factor times as much of itself is X times as bright, and the
    /// air dims it as T_zenith^X. The air's direct extinction runs over the true air mass (about 38 at the horizon)
    /// and alone would leave the horizon black, but the air scatters most of what it takes out of the view back in
    /// from the rest of the glow; the measured net is the extinction over the shell's own X (at most 5.7). Haze and
    /// air enter through the scene's zenith transmittance.
    ///
    /// Colour: the light the air scatters back in is bluer than the direct light it replaces (Rayleigh), so the
    /// glow reddens toward the horizon by GLOW_REDDENING of the direct light's reddening over the same X, the colour
    /// (Rogland's clear night) dark-sky photographs show: a little warm low down, no blue cast and no orange band.
    fn exact(&self, y: f32) -> Vec3f {
        let x = van_rhijn(y, shell_k(CONTINUUM_KM));
        let mut c = self.continuum * self.through(x) * (CONTINUUM_EVEN + (1.0 - CONTINUUM_EVEN) * x);
        for (rgb, k) in &self.lines {
            let x = van_rhijn(y, *k);
            c += *rgb * self.through(x) * x;
        }
        c
    }

    /// What a shell seen through `x` of itself keeps, channel by channel (`exact`).
    fn through(&self, x: f32) -> Vec3f {
        let grey = luminance(self.zenith_t).max(1.0e-6);
        pow3(self.zenith_t / grey, x * GLOW_REDDENING) * grey.powf(x)
    }
}

/// `t` to the power `x`, channel by channel.
fn pow3(t: Vec3f, x: f32) -> Vec3f {
    vec3f(t.x.max(0.0).powf(x), t.y.max(0.0).powf(x), t.z.max(0.0).powf(x))
}

/// The moonlit air: an atmosphere with the moon as its sun, its radiance per unit of the sun's light scaled by the
/// moon's (`scale`, the moon's irradiance above the air over SUN_IRRADIANCE, adaptation included).
struct MoonAir {
    atmo: Atmosphere,
    scale: Vec3f,
}

impl std::fmt::Debug for MoonAir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MoonAir").field("moon_dir", &self.atmo.sun_dir()).field("scale", &self.scale).finish()
    }
}

/// A town's glow, tabulated (see `TownDome::new`).
#[derive(Clone, Debug)]
struct TownDome {
    /// Unit horizontal direction toward the town, (x, z).
    toward: Vec2f,
    /// The natural log of the radiance for the town's unit light, DOME_ROWS_N x DOME_COLUMNS_N, rows over the
    /// elevation (squared from the horizon), columns over the azimuth away from the town (squared from it). The dome
    /// falls off about exponentially, so it is interpolated in logs: linear between rows its core showed rings.
    table: Vec<Vec3f>,
    /// The lamps' colour times the town's light, adaptation included.
    gain: Vec3f,
}

#[derive(Clone, Debug)]
struct Moon {
    /// The moon's own frame (`moon_frame`): its up the moon's north, its right toward its west limb as the sky shows it.
    frame: LightFrame,
    /// The disc's centre direction (the frame's centre), in the map's own frame.
    dir: Vec3f,
    cos_radius: f32,
    tan_radius: f32,
    /// Radiance where the face (`moon_face`) is 1 (a full moon anywhere on the disc),
    /// extinction and adaptation included, and the face's albedo held to its phase's mean (`face_gain`).
    lit: Vec3f,
    /// The generated face, and the sun's direction in the moon's body frame (x east, y north, z toward the Earth).
    face: Arc<MoonFace>,
    sun_body: Vec3f,
    /// The face's Lommel-Seeliger share (`moon_face`, `lunar_lambert_share`).
    ls_share: f32,
    /// Uniform earthshine on the whole disc.
    earthshine: Vec3f,
    /// Mean radiance over the disc.
    mean: Vec3f,
    /// The mean times the disc's solid angle: the disc's whole emission, what the
    /// key carries.
    emission: Vec3f,
    /// The emission as irradiance on a surface facing the moon: it fades out as
    /// the moon sets.
    irradiance: Vec3f,
}

impl NightSky {
    /// `sun_dir` and the moon placement are in the map's own frame.
    /// `star_hours` is the local solar hour that spins the dome (see [`star_hours`])
    /// and `latitude` the sun's: two numbers, each held to its own domain here.
    /// The night is read as clamp() holds it (`NightParams::clamped`, D17), so
    /// its ranges, defaults and NaN rule are `NightParams::clamp_fields`' alone.
    ///
    /// The night's air is the default atmosphere's; [`Self::new_par`] takes the scene's.
    pub fn new(p: &NightParams, seed: u32, sun_dir: Vec3f, star_hours: f32, latitude: f32) -> NightSky {
        let air = AtmosphereParams::default();
        let sun_dir = if sun_dir.is_finite() && sun_dir.length() > 1.0e-6 { sun_dir } else { vec3f(0.0, -1.0, 0.0) };
        let atmo = Atmosphere::new(sun_dir, &air, &SunDiscParams::default());
        NightSky::new_par(p, seed, &atmo, &air, star_hours, latitude, &super::caster::serial_rows).expect("a serial runner runs every job")
    }

    /// `new` in the scene's air: `atmo` is the sun's atmosphere (its direction is the sun's, its transmittance dims
    /// everything from beyond the air) and `air` the parameters it was built from (the moonlit air is built from them,
    /// the moon as its sun). The aurora's footprints are built over `run` (Task 4c's fix round: a held drag of its
    /// Height, Width or Folds builds them on every step); None when the runner skipped a job (an overtaken job's).
    pub fn new_par(p: &NightParams, seed: u32, atmo: &Atmosphere, air: &AtmosphereParams, star_hours: f32, latitude: f32, run: &dyn Fn(usize, &(dyn Fn(usize) + Sync))) -> Option<NightSky> {
        let p = &p.clamped();
        let sun_dir = atmo.sun_dir();
        let sun_elevation_deg = sun_dir.y.clamp(-1.0, 1.0).asin().to_degrees();
        let night_factor = smoothstep(0.0, 12.0, -sun_elevation_deg);
        let lift = adaptation(sun_dir);

        let hours = sane(star_hours, 0.0, 24.0);
        let latitude = sane(latitude, -90.0, 90.0);
        let r = crate::sun::celestial_rows(hours, latitude);
        let rows = [
            vec3f(r[0].x, r[0].y, r[0].z),
            vec3f(r[1].x, r[1].y, r[1].z),
            vec3f(r[2].x, r[2].y, r[2].z),
        ];
        // star_rows are the same rows turned into the galactic frame; its pole row
        // is the galactic pole in world space. Written in star coordinates it is a
        // constant of the sky (the hour turns both frames together).
        let g = crate::sun::star_rows(hours, latitude)[1];
        let g = vec3f(g.x, g.y, g.z);
        let gal_pole = vec3f(rows[0].dot(g), rows[1].dot(g), rows[2].dot(g)).normalize();
        // The galactic centre and l 90 in star space, beside the pole (plan hdri-7 §2): constants of the star sphere,
        // read through the unturned rows as the pole is.
        let galactic = crate::sun::star_rows(hours, latitude);
        let in_star = |w: Vec4f| {
            let w = vec3f(w.x, w.y, w.z);
            vec3f(rows[0].dot(w), rows[1].dot(w), rows[2].dot(w)).normalize()
        };
        let gal_axes = [in_star(galactic[0]), gal_pole, in_star(galactic[2])];
        // The Milky Way turns the whole night sphere, the stars and the band as one (D3); without it the rows are the
        // ones above, untouched.
        let mw = p.milky_way_drawn();
        let rows = match mw {
            Some(m) => night_rows(hours, latitude, m.turn_deg, m.tilt_deg),
            None => rows,
        };

        let star_keep = p.stars * STAR_KEEP_MAX;
        let star_e0 = SUN_IRRADIANCE * 10.0f32.powf(0.4 * SUN_MAGNITUDE);
        let star_gain = star_e0 * 10.0f32.powf(-0.4 * STAR_MAG_LIMIT) * p.star_brightness * lift;
        let mut star_colors = [Vec3f::default(); STAR_COLORS];
        for (i, color) in star_colors.iter_mut().enumerate() {
            // Equal shares of the naked-eye sky's B-V spread, each at its temperature.
            let kelvin = bv_kelvin(bv_quantile((i as f32 + 0.5) / STAR_COLORS as f32));
            let white = Vec3f::all(1.0);
            let c = white + (kelvin_to_rgb(kelvin) - white) * STAR_SATURATION;
            *color = c / luminance(c);
        }

        let extinction = extinction_table(atmo);
        let airglow = Airglow::new(p, lift, &extinction, night_factor > 0.0);

        let mut moon_toa = Vec3f::default();
        let moon = if p.moon {
            let face = moon_face::face(run)?;
            let frame = moon_frame(light_frame(p.moon_azimuth_deg, p.moon_elevation_deg, 0.0), sun_dir, crate::sun::celestial_pole(latitude));
            let radius = (0.5 * p.moon_size_deg).to_radians();
            let dir = frame.center;
            let tint = MOON_TINT / luminance(MOON_TINT);
            let full = tint * (MOON_ALBEDO * SUN_IRRADIANCE / PI * p.moon_brightness * lift);
            // The phase angle, seen from the moon between the sun and us: 0 at full moon.
            let phase_deg = (-dir.dot(sun_dir)).clamp(-1.0, 1.0).acos().to_degrees();
            let ls_share = lunar_lambert_share(phase_deg.min(MOON_PHASE_MAX_DEG));
            let sun_body = vec3f(-sun_dir.dot(frame.right), sun_dir.dot(frame.up), -sun_dir.dot(dir));
            let mut moon = Moon {
                frame,
                dir,
                cos_radius: radius.cos(),
                tan_radius: radius.tan(),
                lit: full * moon_phase_scale(phase_deg, ls_share),
                face,
                sun_body,
                ls_share,
                earthshine: full * (EARTHSHINE_TINT / luminance(EARTHSHINE_TINT)) * earthshine(phase_deg),
                mean: Vec3f::default(),
                emission: Vec3f::default(),
                irradiance: Vec3f::default(),
            };
            moon.lit *= face_gain(&moon);
            // The disc's solid angle 2 pi (1 - cos r), written so it keeps its precision for a tiny r.
            let solid_angle = 4.0 * PI * (0.5 * radius).sin().powi(2);
            // Above the air first: what the moonlit air is lit by. The disc is linear in its light, so the air's
            // transmittance along the moon scales the mean as it scales the light.
            let mean_toa = disc_mean(&moon);
            moon_toa = mean_toa * solid_angle;
            let t = extinction_at(&extinction, dir.y);
            moon.lit *= t;
            moon.earthshine *= t;
            moon.mean = mean_toa * t;
            moon.emission = moon.mean * solid_angle;
            moon.irradiance = moon.mean * (solid_angle * smoothstep(-0.01, 0.01, dir.y));
            Some(moon)
        } else {
            None
        };
        let moon_air = match &moon {
            Some(m) if night_factor > 0.0 && m.dir.y > MOON_AIR_MIN_SIN && moon_toa.max_elem() > 0.0 => {
                let hidden = SunDiscParams { visible: false, ..SunDiscParams::default() };
                Some(Arc::new(MoonAir { atmo: Atmosphere::new(m.dir, air, &hidden), scale: moon_toa / SUN_IRRADIANCE }))
            }
            _ => None,
        };
        let town = match p.light_pollution_drawn().filter(|_| night_factor > 0.0) {
            Some(lp) => Some(TownDome::new(lp, air, lift, run)?),
            None => None,
        };

        // Plan hdri-7 §2: the band shows from astronomical twilight on, lifted as the stars are.
        let astro = smoothstep(ASTRO_FADE_DEG.0, ASTRO_FADE_DEG.1, -sun_elevation_deg);
        // Task 3b: the nebulas' Realism sets the whole deep sky's look, the band's too; read from their block whether it
        // draws or not (an off block keeps the look it was set to), its default without one.
        let realism = p.nebulas.as_ref().map_or(NebulaParams::default().realism, |n| n.realism);
        let milky_way = mw.map(|m| MilkyWay::new(m, seed, gal_axes, m.strength * lift * astro, realism));
        // Plan hdri-7 §3: the nebulas likewise, along the band when it draws; their stars lifted as the field's are.
        let nebulas = p.nebulas_drawn().map(|n| Nebulas::new(n, mw.map(|_| gal_axes), lift * astro, star_gain * astro, &star_colors));
        // Plan hdri-7 §4: the aurora fades with the night as the stars do; built only when it draws and the sun is down.
        let aurora_layer = match p.aurora_drawn().filter(|_| night_factor > 0.0) {
            Some(a) => Some(Aurora::new_par(a, a.strength * lift * night_factor, run)?),
            None => None,
        };

        let mut sky = NightSky {
            night_factor,
            sun_dir,
            sun_elevation_deg,
            rows,
            gal_pole,
            star_keep,
            star_keep_max: (star_keep * (1.0 + MILKY_WAY_BOOST)).min(1.0),
            star_gain,
            star_seed: hash_u32(seed ^ 0x5354_4152),
            star_colors,
            extinction,
            airglow,
            moon_air,
            town,
            moon,
            ambient: Vec3f::default(),
            ground_irradiance: Vec3f::default(),
            moon_ground: Vec3f::default(),
            gal_axes,
            milky_way,
            nebulas,
            aurora_layer,
            horizon_sin: match atmo.horizon_dip() {
                dip if dip > 0.0 => -dip.sin(),
                _ => 0.0,
            },
        };
        let (ambient, sky_irradiance) = sky.integrate_glow();
        let moon_direct = sky.moon.as_ref().map_or(Vec3f::default(), |m| m.irradiance * m.dir.y.max(0.0));
        sky.ambient = ambient;
        sky.moon_ground = moon_direct * night_factor;
        sky.ground_irradiance = sky_irradiance + sky.moon_ground;
        // The aurora lights the scene (plan hdri-7 D9): its mean over the sky (the clouds' ambient) and its light on
        // flat ground join the glow's, only when it draws, so a night without it is what it was.
        if sky.aurora_layer.is_some() {
            let (mean, irradiance) = sky.integrate_aurora();
            sky.ambient += mean;
            sky.ground_irradiance += irradiance;
        }
        Some(sky)
    }

    /// 0 by day, rising from sunset to 1 at the end of nautical twilight (sun at -12 degrees).
    pub fn night_factor(&self) -> f32 {
        self.night_factor
    }

    /// Starlight arriving from `dir`, times the night factor. Near the horizon it is
    /// dimmed and reddened by the air, and below it (`horizon_sin`) there is none.
    pub fn stars(&self, dir: Vec3f) -> Vec3f {
        if self.night_factor <= 0.0 || self.star_keep <= 0.0 || self.star_gain <= 0.0 || !(dir.y > self.horizon_sin) {
            return Vec3f::default();
        }
        let field = self.star_field(self.to_star(dir));
        if field.max_elem() <= 0.0 {
            return field;
        }
        field * self.transmittance(dir.y) * (self.night_factor * smoothstep(0.0, 0.03, dir.y - self.horizon_sin))
    }

    /// What rides with the points (`Env::sky_layers`' `points`): the stars and, with the Milky Way or the nebulas drawn,
    /// the band and the clouds of gas, the stars behind both dusts and the band behind the clouds'. `self.stars(dir)` bit
    /// for bit when neither draws. `texel`: the lookup's texel in radians (0: exact, the finest detail). Plan hdri-7 D9.
    pub fn deep_sky(&self, dir: Vec3f, texel: f32) -> Vec3f {
        if self.milky_way.is_none() && self.nebulas.is_none() {
            return self.stars(dir);
        }
        if self.night_factor <= 0.0 || !(dir.y > self.horizon_sin) {
            return Vec3f::default();
        }
        let s = self.to_star(dir);
        let field = if self.star_keep > 0.0 && self.star_gain > 0.0 { self.star_field(s) } else { Vec3f::default() };
        let band = self.milky_way.as_ref().map_or(Vec3f::default(), |m| m.at(s, texel).0);
        let (gas, veil) = self.nebulas.as_ref().map_or((Vec3f::default(), 1.0), |n| n.at(s, texel));
        // Only a lookup a star reaches pays for the dust in front of it.
        let dust = if field.max_elem() > 0.0 { self.dust_before_stars(s) } else { 1.0 };
        // The stars as `stars` writes them (so a dust of 1 and no band are `stars`, bit for bit), behind both dusts.
        let (extinction, horizon) = (self.transmittance(dir.y), smoothstep(0.0, 0.03, dir.y - self.horizon_sin));
        let stars = field * extinction * (self.night_factor * horizon);
        stars * dust + (band * veil + gas) * extinction * horizon
    }

    /// The share of a star's light at star-space `s` the dusts let through, the Milky Way's and the nebulas' (1 without
    /// them). A star is a point, not a texture to band-limit, so the dust is read at full detail (texel 0): a star behind
    /// it dims alike at every width. Read at the lookup's texel, the Milky Way's coarse octaves dimmed the stars of a held
    /// bake differently from a released one's (a block's share differed by 34 %, 512 against 1024; 23 %, 1024 against
    /// 2048).
    fn dust_before_stars(&self, s: Vec3f) -> f32 {
        let band = self.milky_way.as_ref().map_or(1.0, |m| m.transmittance(s, 0.0));
        self.nebulas.as_ref().map_or(band, |n| band * n.transmittance(s, 0.0))
    }

    /// The share of the stars' light along `dir` that `deep_sky` lets through: the Milky Way's dust times the nebulas'
    /// (1 without either, off their dust and below the horizon). The width tests compare it as a field (`widths_agree`).
    #[cfg(test)]
    pub(super) fn star_transmittance(&self, dir: Vec3f) -> f32 {
        if dir.y > self.horizon_sin { self.dust_before_stars(self.to_star(dir)) } else { 1.0 }
    }

    /// The nebulas' embedded stars along `dir` as `deep_sky` adds them (none without the nebulas): stars that Star
    /// brightness scales and no dust dims, so `widths_agree` counts them beside the field's.
    #[cfg(test)]
    pub(super) fn embedded_stars(&self, dir: Vec3f) -> Vec3f {
        match &self.nebulas {
            Some(n) if self.night_factor > 0.0 && dir.y > self.horizon_sin => {
                n.embedded_stars(self.to_star(dir)) * self.transmittance(dir.y) * smoothstep(0.0, 0.03, dir.y - self.horizon_sin)
            }
            _ => Vec3f::default(),
        }
    }

    /// Where the night's features are, in the map's own frame (plan hdri-7: the tests' and the screenshots' aim).
    pub fn features(&self) -> NightFeatures {
        NightFeatures {
            galactic_centre: self.milky_way.as_ref().map(|_| self.to_world(self.gal_axes[0])),
            nebulas: self.nebulas.as_ref().map_or(Vec::new(), |n| n.centres().into_iter().map(|(c, r)| (self.to_world(c), r)).collect()),
            aurora_middle: self.aurora_layer.as_ref().map(|a| a.middle()),
        }
    }

    /// Whether the aurora draws: its block on and the sun below the horizon. `Env::sky_layers` and `dome_light` add
    /// nothing otherwise (plan hdri-7).
    pub fn has_aurora(&self) -> bool {
        self.aurora_layer.is_some()
    }

    /// The aurora along `dir` (map frame): the night's lift and fade in, dimmed and reddened toward the horizon as the
    /// stars are; 0 below it and off its arc. `texel` band-limits its folds and rays (AURORA_BARE_TEXEL for a bare
    /// lookup). Diffuse light, never a key (plan 1b D1).
    pub fn aurora(&self, dir: Vec3f, texel: f32) -> Vec3f {
        let Some(a) = &self.aurora_layer else { return Vec3f::default() };
        if !(dir.y > 0.0) {
            return Vec3f::default();
        }
        let c = a.at(dir, texel);
        if !(c.max_elem() > 0.0) {
            return Vec3f::default();
        }
        c * self.transmittance(dir.y) * smoothstep(0.0, 0.03, dir.y)
    }

    /// The moon's disc: lit from the sun's direction, plus earthshine. It is also drawn
    /// by day (physically faint), and the ground hides it once it sets (under `horizon_sin`).
    /// Its face at its finest; [`Self::moon_at`] filters it over a lookup's texel.
    pub fn moon(&self, dir: Vec3f) -> Vec3f {
        self.moon_at(dir, 0.0)
    }

    /// [`Self::moon`] for a lookup `texel` wide (radians): the face filtered over the share of it the bake's sampling
    /// leaves to the lookup (MOON_FOOTPRINT_REFINED, MOON_FOOTPRINT), so the maria hold from a dozen texels across to
    /// a few hundred without aliasing.
    pub fn moon_at(&self, dir: Vec3f, texel: f32) -> Vec3f {
        let Some(m) = &self.moon else { return Vec3f::default() };
        if !(dir.y > self.horizon_sin) || !(dir.dot(m.dir) >= m.cos_radius) {
            return Vec3f::default();
        }
        let Some(p) = project(&m.frame, dir) else { return Vec3f::default() };
        let share = if m.tan_radius.atan() < crate::HOT_MAX_TEXELS * texel { MOON_FOOTPRINT_REFINED } else { MOON_FOOTPRINT };
        let footprint = share * texel.max(0.0) / m.tan_radius;
        moon_shade(m, vec2f(p.x / m.tan_radius, p.y / m.tan_radius), footprint)
    }

    /// The natural glow seen through the air, the moonlit air and a town's glow. Times the night factor. It fades
    /// into the ground over about 2 degrees below the horizon (`horizon_sin`; between level and a dipped horizon it
    /// holds the glow at level).
    pub fn glow(&self, dir: Vec3f) -> Vec3f {
        if self.night_factor <= 0.0 {
            return Vec3f::default();
        }
        let fade = smoothstep(-0.03, 0.0, dir.y - self.horizon_sin);
        if !(fade > 0.0) {
            return Vec3f::default();
        }
        self.air(dir, dir.y.max(0.0)) * (self.night_factor * fade)
    }

    /// The air's transmittance from the viewer to space at elevation sine `y` (the horizon's below it): what dims
    /// and reddens everything from beyond the air.
    fn transmittance(&self, y: f32) -> Vec3f {
        extinction_at(&self.extinction, y)
    }

    /// The night air's light at the horizon along `dir`'s azimuth (the airglow there and the moonlit air, as
    /// [`Self::glow`] has them at the horizon, before the night factor and with no fade): what a floor's ground
    /// blends into by the air's transmittance along the look (`Env::floor_ground`) in place of the glow's fade, so a
    /// lit road runs on to the horizon by night and meets the sky there.
    pub fn glow_at_horizon(&self, dir: Vec3f) -> Vec3f {
        if self.night_factor <= 0.0 {
            return Vec3f::default();
        }
        self.air(dir, 0.0) * self.night_factor
    }

    /// The glow before the night factor along `dir`'s azimuth at elevation sine `y` (>= 0): the natural glow
    /// through the air, the moonlit air and a town's glow.
    fn air(&self, dir: Vec3f, y: f32) -> Vec3f {
        let mut c = self.airglow.seen(y);
        if let Some(m) = &self.moon_air {
            c += m.atmo.sky(vec3f(dir.x, y, dir.z)) * m.scale;
        }
        if let Some(t) = &self.town {
            c += t.at(dir, y);
        }
        c
    }

    /// The ground below the horizon, lit by the night sky and the moon: a Lambert
    /// `ground_color` under `ground_irradiance`. It adds to the atmosphere's own
    /// ground, which is black at night. Toward the horizon it fades as the glow fades in.
    pub fn ground(&self, dir: Vec3f, ground_color: Vec3f) -> Vec3f {
        if !(dir.y < 0.0) {
            return Vec3f::default();
        }
        let below = 1.0 - smoothstep(-0.03, 0.0, dir.y - self.horizon_sin);
        ground_color * self.ground_irradiance * (below / PI)
    }

    /// The night's irradiance on flat ground (its sky, the moon and the aurora), which [`Self::ground`] lights its
    /// ground colour with.
    pub fn ground_irradiance(&self) -> Vec3f {
        self.ground_irradiance
    }

    /// [`Self::ground`] for a ground whose albedo times irradiance is `lit` (the floor's planes, plan hdri-8 T2):
    /// `ground(dir, c)` is `ground_lit(dir, c * ground_irradiance())`, bit for bit.
    pub fn ground_lit(&self, dir: Vec3f, lit: Vec3f) -> Vec3f {
        if !(dir.y < 0.0) {
            return Vec3f::default();
        }
        let below = 1.0 - smoothstep(-0.03, 0.0, dir.y - self.horizon_sin);
        lit * (below / PI)
    }

    /// The light the moon's disc puts on flat ground (its part of the ground's
    /// irradiance, by the night factor): what a cloud deck in front of the
    /// moon takes off the ground (the weather's ground, D7).
    pub fn moon_ground_irradiance(&self) -> Vec3f {
        self.moon_ground
    }

    /// Mean radiance of the night sky over the upper hemisphere, glow plus moonlit air.
    /// It is the ambient term for the clouds.
    pub fn ambient(&self) -> Vec3f {
        self.ambient
    }

    /// The moon as the map's key light. It applies only when the moon is enabled and
    /// above the horizon and the sun is below -6 degrees; above that the twilight sky
    /// still outshines it.
    /// - The radiance is the disc's emission (its mean over its true solid angle)
    ///   over the cone its f32 `cos_radius` names, the cone every consumer of the
    ///   key computes, so radiance x 2 pi (1 - cos_radius) is its true irradiance
    ///   at any size (`EnvSun::cone_radiance`).
    /// - The direction is in the map's own frame.
    pub fn moon_key(&self) -> Option<EnvSun> {
        let m = self.moon.as_ref()?;
        if !(self.sun_elevation_deg < -6.0) || !(m.dir.y > self.horizon_sin) {
            return None;
        }
        // A disc a fraction of a degree wide with a hard edge: a surface facing
        // it gets all of its emission, and the disc is its own covering cone.
        Some(EnvSun { dir: m.dir, radiance: EnvSun::cone_radiance(m.emission, m.cos_radius), cos_radius: m.cos_radius, facing: 1.0, cos_cover: m.cos_radius })
    }

    /// The moon's disc as [`Self::moon`] draws it, for a viewer that draws it itself (`Env::sky_discs`): None without a
    /// moon. In the map's own frame.
    pub fn moon_look(&self) -> Option<MoonDiscLook> {
        self.moon.as_ref().map(|m| MoonDiscLook {
            dir: m.dir,
            right: m.frame.right,
            up: m.frame.up,
            tan_radius: m.tan_radius,
            limb: MOON_LIMB,
            lit: m.lit,
            ls_share: m.ls_share,
            earthshine: m.earthshine,
            sun_dir: self.sun_dir,
        })
    }

    /// The moon's disc for the bake's refinement: its direction in the map's own frame and its angular radius.
    pub fn moon_disc(&self) -> Option<(Vec3f, f32)> {
        self.moon.as_ref().map(|m| (m.dir, m.tan_radius.atan()))
    }

    /// World direction to star space (the rows are orthonormal).
    fn to_star(&self, dir: Vec3f) -> Vec3f {
        vec3f(self.rows[0].dot(dir), self.rows[1].dot(dir), self.rows[2].dot(dir))
    }

    /// Star space back to the world: the transpose of the rows.
    fn to_world(&self, s: Vec3f) -> Vec3f {
        self.rows[0] * s.x + self.rows[1] * s.y + self.rows[2] * s.z
    }

    /// Sum of the star splats around unit star-space direction `s`, gathered over the
    /// 3x3x3 grid cells around the cell `s` falls in (see the module notes on why
    /// that block always holds every star within reach).
    fn star_field(&self, s: Vec3f) -> Vec3f {
        let base = [
            (s.x * STAR_GRID).floor() as i32,
            (s.y * STAR_GRID).floor() as i32,
            (s.z * STAR_GRID).floor() as i32,
        ];
        let mut sum = Vec3f::default();
        for dz in -1..=1 {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let Some(star) = self.star_in_cell([base[0] + dx, base[1] + dy, base[2] + dz]) else { continue };
                    // The perpendicular distance, sin of the angle to the star: the
                    // shader's length(cross(sd, star)).
                    let d2 = Vec3f::cross(s, star.dir).length_squared();
                    if !(d2 < STAR_REACH2) {
                        continue;
                    }
                    sum += star.color * (star.flux * star_kernel(d2));
                }
            }
        }
        sum
    }

    /// The star in grid cell `c`, or None for an empty cell. Streams 0..6 of the
    /// cell's hash give: the density roll, the jitter (x, y, z), the magnitude and the colour.
    fn star_in_cell(&self, c: [i32; 3]) -> Option<Star> {
        // The density roll first, against the densest the field can be, so most
        // empty cells cost a single hash.
        let roll = self.cell_hash(c, 0);
        if roll >= self.star_keep_max {
            return None;
        }
        // The jittered point, as the shader's c + k·0.5 + 0.25: always inside the cell.
        let p = vec3f(
            c[0] as f32 + 0.25 + 0.5 * self.cell_hash(c, 1),
            c[1] as f32 + 0.25 + 0.5 * self.cell_hash(c, 2),
            c[2] as f32 + 0.25 + 0.5 * self.cell_hash(c, 3),
        );
        let r = p.length();
        // Only the cells the sphere passes through hold a star.
        if !((r - STAR_GRID).abs() <= STAR_SHELL) {
            return None;
        }
        let dir = p / r;
        // The Milky Way: a denser field near the galactic equator.
        let sin_b = self.gal_pole.dot(dir) / MILKY_WAY_SIN_HALF;
        let keep = self.star_keep * (1.0 + MILKY_WAY_BOOST * (-sin_b * sin_b).exp());
        if roll >= keep {
            return None;
        }
        // Magnitude LIMIT + 2·log10(h): the flux is h^-0.8 faintest stars.
        let h = self.cell_hash(c, 4).max(STAR_HASH_FLOOR);
        let i = (self.cell_hash(c, 5) * STAR_COLORS as f32) as usize;
        Some(Star { dir, flux: self.star_gain * h.powf(-0.8), color: self.star_colors[i.min(STAR_COLORS - 1)] })
    }

    /// A uniform value in [0, 1) for a cell and a stream index, from the seeded hash.
    fn cell_hash(&self, c: [i32; 3], stream: u32) -> f32 {
        hash01(c[0], c[1], c[2], self.star_seed.wrapping_add(stream))
    }

    /// Integrates the glow over the upper hemisphere by midpoint quadrature. Returns:
    /// - the glow's mean radiance there (for cloud lighting);
    /// - the irradiance it puts on flat ground.
    ///
    /// The Mie aureole is narrower than a quadrature cell, but it is a small share of either total.
    fn integrate_glow(&self) -> (Vec3f, Vec3f) {
        const THETA_STEPS: usize = 16;
        const PHI_STEPS: usize = 32;
        let d_theta = FRAC_PI_2 / THETA_STEPS as f32;
        let d_phi = TAU / PHI_STEPS as f32;
        let mut mean = Vec3f::default();
        let mut irradiance = Vec3f::default();
        for i in 0..THETA_STEPS {
            let (sin_t, cos_t) = ((i as f32 + 0.5) * d_theta).sin_cos();
            for j in 0..PHI_STEPS {
                let (sin_p, cos_p) = ((j as f32 + 0.5) * d_phi).sin_cos();
                let l = self.glow(vec3f(sin_t * cos_p, cos_t, sin_t * sin_p));
                let d_omega = sin_t * d_theta * d_phi;
                mean += l * d_omega;
                irradiance += l * (d_omega * cos_t);
            }
        }
        (mean / TAU, irradiance)
    }

    /// The aurora's mean radiance over the upper hemisphere and the irradiance it puts on flat ground, at the bare
    /// lookups' band limit, over the dome's looks (`dome_look`, each an equal share of the hemisphere): the same sums
    /// the map's dome makes of it, so the ground under a deck gains just what the deck's light gains (D9; Task 4c: the
    /// glow's 16 x 32 midpoint quadrature sampled other directions, and against the folded ribbons the two sums differed
    /// by up to 15 %).
    fn integrate_aurora(&self) -> (Vec3f, Vec3f) {
        let count = DOME_ROWS * DOME_COLUMNS;
        let share = TAU / count as f32;
        let (mut mean, mut irradiance) = (Vec3f::default(), Vec3f::default());
        for j in 0..DOME_ROWS {
            for i in 0..DOME_COLUMNS {
                let (d, sine) = dome_look(j, i);
                let l = self.aurora(d, AURORA_BARE_TEXEL);
                mean += l;
                irradiance += l * (sine * share);
            }
        }
        (mean / count as f32, irradiance)
    }
}

/// Where the night's features are, in the map's own frame (before `rotation_deg`): the galactic centre (the Milky
/// Way drawn, turned with the sphere), each visible nebula's centre and angular radius (radians), and the aurora's
/// arc middle at its lower border. Plan hdri-7.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NightFeatures {
    pub galactic_centre: Option<Vec3f>,
    pub nebulas: Vec<(Vec3f, f32)>,
    pub aurora_middle: Option<Vec3f>,
}

/// World to star space for the solar hour and latitude with the night sphere turned by `turn_deg` about the
/// celestial pole (a sidereal offset: a season; +116 puts the galactic centre on the meridian at 22 h) and tilted by
/// `tilt_deg` about the observer's east-west line (+X; positive raises the north). At 0 and 0 these are
/// `celestial_rows`' rows, bit for bit. Plan hdri-7 D3.
fn night_rows(hours: f32, latitude: f32, turn_deg: f32, tilt_deg: f32) -> [Vec3f; 3] {
    let hours = if turn_deg != 0.0 { (hours + turn_deg / 15.0).rem_euclid(24.0) } else { hours };
    let r = crate::sun::celestial_rows(hours, latitude);
    let rows = [vec3f(r[0].x, r[0].y, r[0].z), vec3f(r[1].x, r[1].y, r[1].z), vec3f(r[2].x, r[2].y, r[2].z)];
    if tilt_deg == 0.0 {
        return rows;
    }
    // A star at s shows at the world's R^T s; the sky tilted by T about +X shows it at Rx(T) R^T s, so each row r
    // (r . d) becomes Rx(T) r.
    let (s, c) = tilt_deg.to_radians().sin_cos();
    rows.map(|r| vec3f(r.x, r.y * c - r.z * s, r.y * s + r.z * c))
}

/// Normalised star profile at squared perpendicular distance `d2` (sin² of the
/// angle; radians² to within 1e-4 over the reach).
///
/// It integrates to 1 over the sphere, so a star's flux is its irradiance
/// whatever the sigmas are. The core carries STAR_CORE_SHARE and the halo the rest.
pub(super) fn star_kernel(d2: f32) -> f32 {
    STAR_CORE_SHARE * windowed_gaussian(d2, STAR_SIGMA_CORE)
        + (1.0 - STAR_CORE_SHARE) * windowed_gaussian(d2, STAR_SIGMA_HALO)
}

/// A Gaussian cut at 3 sigma, lowered to meet zero there, and normalised to unit mass.
fn windowed_gaussian(d2: f32, sigma: f32) -> f32 {
    let two_s2 = 2.0 * sigma * sigma;
    let x = d2 / two_s2;
    if x >= 4.5 {
        return 0.0;
    }
    ((-x).exp() - GAUSS_FLOOR) / (PI * two_s2 * GAUSS_MASS)
}

/// The moon's disc as `NightSky::moon` draws it ([`NightSky::moon_look`]): along `d` (above the horizon), with
/// `q = (d . right, d . up) / (d . dir) / tan_radius`, r = |q| < 1, the radiance is [`MoonDiscLook::shade`] times
/// `1 - smoothstep(1 - limb, 1, r)`: `(lit x face + earthshine) x albedo`, where face is [`moon_face`],
/// ls_share x 2 cos i / (cos i + cos e) + (1 - ls_share) x cos i for cos i > 0 (else 0), read on the normal the
/// generated face's relief bends (`moon_face::relief_normal`) and let in over the footprint from the terminator and by
/// the bent facet's share of its texel's apparent area, and albedo is the face's (`moon_face::MoonFace::sample` at the
/// body point (-qx, qy), its colour along `moon_face::COLOR_AXIS`). `right` and `up` are the moon's own frame: up its
/// north, right its west limb.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MoonDiscLook {
    pub dir: Vec3f,
    pub right: Vec3f,
    pub up: Vec3f,
    pub tan_radius: f32,
    pub limb: f32,
    pub lit: Vec3f,
    /// The face's Lommel-Seeliger share (the lunar-Lambert L): 1 at full moon, 0.2 at a quarter, 0 past 120 degrees.
    pub ls_share: f32,
    pub earthshine: Vec3f,
    pub sun_dir: Vec3f,
}

impl MoonDiscLook {
    /// The face at a point of the disc with these incidence and emission cosines ([`moon_face`]).
    pub fn face(&self, cos_i: f32, cos_e: f32) -> f32 {
        moon_face(self.ls_share, cos_i, cos_e)
    }

    /// The radiance at disc coordinates `q` (in radii, along `right` and `up`), the face filtered over `footprint`
    /// (radii): what the preview's shader draws there, before its limb is box-filtered over the pixel. The process's
    /// face (`moon_face::built_face`, which the bake that made this look built); the even face without one.
    pub fn shade(&self, q: Vec2f, footprint: f32) -> Vec3f {
        let r2 = q.x * q.x + q.y * q.y;
        if r2 >= 1.0 {
            return Vec3f::default();
        }
        let dir = self.dir.normalize();
        let cos_e = (1.0 - r2).sqrt();
        let normal = self.right * q.x + self.up * q.y - dir * cos_e;
        let sun_body = vec3f(-self.sun_dir.dot(self.right), self.sun_dir.dot(self.up), -self.sun_dir.dot(dir));
        let (lit, albedo) = face_light(moon_face::built_face().as_deref(), self.ls_share, sun_body, normal.dot(self.sun_dir), q, cos_e, footprint);
        (self.lit * lit + self.earthshine) * albedo
    }
}

/// The moon's face, what its light is spread by over the disc (see HAPKE_W and FACE_FIT_N): the lunar-Lambert mix
/// `ls_share` x Lommel-Seeliger 2 cos i / (cos i + cos e) + (1 - `ls_share`) x Lambert cos i, 0 where the surface turns
/// away from the sun (cos i <= 0). Both parts are 1 at the centre of a full moon.
pub fn moon_face(ls_share: f32, cos_i: f32, cos_e: f32) -> f32 {
    if !(cos_i > 0.0) {
        return 0.0;
    }
    ls_share * (2.0 * cos_i / (cos_i + cos_e.max(0.0))) + (1.0 - ls_share) * cos_i
}

/// The light law on the generated face at disc coordinates `q` (in radii along the moon's frame), whose sphere point
/// faces the viewer by `cos_e` and the sun by `cos_i`: the face's light (`moon_face`) on the normal its relief bends,
/// and its albedo per channel, the face read over `footprint` (radii). The relief lights only the sunlit side: over the
/// footprint from the terminator the light passes from the smooth sphere's law to the relief's, so a rim never shows
/// beyond the terminator, the terminator does not alias, and the band loses no light (a crescent a few texels wide keeps
/// its whole light). The bent facet counts by its share of its texel's apparent area, n_z / ((n . p) cos e) held to
/// 0..1 (its area over the texel's, 1 / (n . p), times its view cosine over the sphere's): a slope turned toward the
/// limb is foreshortened and the rest of its texel shows the surface round it, the smooth sphere's light; a facet
/// turned from the viewer (n_z <= 0: the inner wall on the disc-centre side of a crater near the limb) is hidden and
/// draws the smooth law, never black at full moon nor at the law's grazing maximum. At the disc's centre, and wherever
/// a facet faces the viewer at least as much as the sphere does, it is 1. `sun_body` is the sun in the body frame (x
/// east, y north, z toward the Earth); the face's x east is the frame's -right. Without a face, the even face of 1.
fn face_light(face: Option<&MoonFace>, ls_share: f32, sun_body: Vec3f, cos_i: f32, q: Vec2f, cos_e: f32, footprint: f32) -> (f32, Vec3f) {
    let Some(face) = face else {
        return (moon_face(ls_share, cos_i, cos_e), Vec3f::all(1.0));
    };
    let (x, y) = (-q.x, q.y);
    let t = face.sample(x, y, footprint);
    let n = moon_face::relief_normal(x, y, cos_e, t[2], t[3]);
    let bent = n[0] * sun_body.x + n[1] * sun_body.y + n[2] * sun_body.z;
    let (relief, smooth) = (moon_face(ls_share, bent, n[2]), moon_face(ls_share, cos_i, cos_e));
    let cover = (n[2] / ((n[0] * x + n[1] * y + n[2] * cos_e) * cos_e).max(1.0e-6)).clamp(0.0, 1.0);
    let lit = smooth + (relief - smooth) * smoothstep(0.0, footprint.max(1.0e-4), cos_i) * cover;
    let a = moon_face::albedo_rgb(t);
    (lit, vec3f(a[0], a[1], a[2]))
}

/// Radiance of the moon at disc coordinates `q`, the face filtered over `footprint` (both in radii).
///
/// `q` is the tangent-plane offset divided by tan(radius) along the moon's own frame: the unit disc is the moon. The
/// moon is far away, so the visible hemisphere is seen orthographically, and the sun lights it from `m.sun_body`.
fn moon_shade(m: &Moon, q: Vec2f, footprint: f32) -> Vec3f {
    let r2 = q.x * q.x + q.y * q.y;
    if r2 >= 1.0 {
        return Vec3f::default();
    }
    // Cosine of the angle toward the viewer, and the smooth sphere's cosine toward the sun.
    let cos_e = (1.0 - r2).sqrt();
    let cos_i = -q.x * m.sun_body.x + q.y * m.sun_body.y + cos_e * m.sun_body.z;
    // The face: flat across a full moon, falling to 0 at the terminator, where the surface turns away from the sun.
    let (lit, albedo) = face_light(Some(&m.face), m.ls_share, m.sun_body, cos_i, q, cos_e, footprint);
    let limb = 1.0 - smoothstep(1.0 - MOON_LIMB, 1.0, r2.sqrt());
    (m.lit * lit + m.earthshine) * albedo * limb
}

/// The cells across the grid `disc_mean` and `face_gain` read the disc on; the face is filtered over a cell.
const DISC_GRID: usize = 24;

/// Mean radiance over the moon's disc, on a DISC_GRID x DISC_GRID grid of the unit disc. The key light and the moon's
/// irradiance therefore agree with what `moon()` draws.
fn disc_mean(m: &Moon) -> Vec3f {
    let cell = 2.0 / DISC_GRID as f32;
    let mut sum = Vec3f::default();
    let mut count = 0usize;
    for j in 0..DISC_GRID {
        for i in 0..DISC_GRID {
            let q = vec2f((i as f32 + 0.5) * cell - 1.0, (j as f32 + 0.5) * cell - 1.0);
            if q.x * q.x + q.y * q.y < 1.0 {
                sum += moon_shade(m, q, cell);
                count += 1;
            }
        }
    }
    sum / count.max(1) as f32
}

/// What the lit face is scaled by, per channel, so its mean over the disc at this phase is the even face's: the
/// albedo's mean is 1 over the whole disc, but a phase lights only part of it (a quarter's half holds more maria or
/// fewer), and the relief takes a little. The disc's whole light then stays Allen's, and the key, the moonlit air and
/// the ground's light are what they were.
fn face_gain(m: &Moon) -> Vec3f {
    let cell = 2.0 / DISC_GRID as f32;
    let (mut plain, mut faced) = (0.0f32, Vec3f::default());
    for j in 0..DISC_GRID {
        for i in 0..DISC_GRID {
            let q = vec2f((i as f32 + 0.5) * cell - 1.0, (j as f32 + 0.5) * cell - 1.0);
            let r2 = q.x * q.x + q.y * q.y;
            if r2 < 1.0 {
                let cos_e = (1.0 - r2).sqrt();
                let cos_i = -q.x * m.sun_body.x + q.y * m.sun_body.y + cos_e * m.sun_body.z;
                let limb = 1.0 - smoothstep(1.0 - MOON_LIMB, 1.0, r2.sqrt());
                let (lit, albedo) = face_light(Some(&m.face), m.ls_share, m.sun_body, cos_i, q, cos_e, cell);
                plain += moon_face(m.ls_share, cos_i, cos_e) * limb;
                faced += albedo * (lit * limb);
            }
        }
    }
    let gain = |f: f32| if f > 1.0e-12 { plain / f } else { 1.0 };
    vec3f(gain(faced.x), gain(faced.y), gain(faced.z))
}

/// The moon's own frame: `base` (its centre the moon's direction) turned so its up is the moon's north and its right
/// the moon's west limb as the sky shows it (`light_frame`'s right lies on the viewer's left; the IAU east, Mare
/// Crisium's limb, on the viewer's right). The Moon's axis is square to the plane of the sun, the Earth and the moon
/// (its equator holds the sun to 1.5 degrees and the Earth to its libration), on the side of the celestial north pole
/// `pole`; as that plane narrows toward full or new moon the pole's direction on the sky takes over (MOON_POLE_HOLD).
/// The plane's normal enters squared (times its own dot with the pole), so the north turns smoothly through every
/// placing. `base` when neither gives a direction (the moon at the pole, at full moon).
fn moon_frame(base: LightFrame, sun_dir: Vec3f, pole: Vec3f) -> LightFrame {
    let dir = base.center;
    let plane = Vec3f::cross(sun_dir, dir);
    let pole_on_sky = pole - dir * dir.dot(pole);
    let north = plane * plane.dot(pole) + pole_on_sky * MOON_POLE_HOLD;
    let north = north - dir * dir.dot(north);
    if !(north.length() > 1.0e-6) {
        return base;
    }
    let up = north.normalize();
    LightFrame { center: dir, right: Vec3f::cross(up, dir), up, face: None }
}

/// Airglow brightening toward the horizon (van Rhijn). A thin emitting shell seen at
/// zenith angle z is looked through 1/sqrt(1 - K·sin²z) times as long; y = cos z.
fn van_rhijn(y: f32, k: f32) -> f32 {
    1.0 / (1.0 - k * (1.0 - y * y)).max(1.0e-4).sqrt()
}

/// Van Rhijn's K for a shell `km` up: (R / (R + h))².
fn shell_k(km: f32) -> f32 {
    let r = EARTH_RADIUS_KM / (EARTH_RADIUS_KM + km);
    r * r
}

/// The air's transmittance from the viewer to space, tabulated over sqrt(sin elevation) from the horizon to the
/// zenith (the atmosphere is the same in every azimuth).
fn extinction_table(atmo: &Atmosphere) -> [Vec3f; EXTINCTION_ROWS] {
    std::array::from_fn(|i| {
        let u = i as f32 / (EXTINCTION_ROWS - 1) as f32;
        let y = u * u;
        atmo.transmittance(vec3f((1.0 - y * y).max(0.0).sqrt(), y, 0.0))
    })
}

/// The table at elevation sine `y`, linear between rows; the horizon's row below it.
fn extinction_at(table: &[Vec3f; EXTINCTION_ROWS], y: f32) -> Vec3f {
    sqrt_rows_at(table, y)
}

/// A table over sqrt(sin elevation), its first row the horizon's and its last the zenith's, at elevation sine `y`,
/// linear between rows; the horizon's row below it.
fn sqrt_rows_at<const N: usize>(table: &[Vec3f; N], y: f32) -> Vec3f {
    let u = if y > 0.0 { y.min(1.0).sqrt() * (N - 1) as f32 } else { 0.0 };
    let i = (u as usize).min(N - 2);
    let f = u - i as f32;
    table[i] + (table[i + 1] - table[i]) * f
}

impl Airglow {
    /// The glow for `p` (as clamp() holds it), lifted by `lift`, seen through the air `extinction` tabulates. Its table
    /// (half a millisecond) is filled only `by_night`: by day nothing reads it (`NightSky::glow`) and it stays black.
    fn new(p: &NightParams, lift: f32, extinction: &[Vec3f; EXTINCTION_ROWS], by_night: bool) -> Airglow {
        let strength = p.glow_strength * lift;
        // The glow colour tints the whole glow, the lines and the continuum alike, channel by channel against the
        // default (at it the tint is 1 exactly; black turns the glow off).
        let d = DEFAULT_GLOW_COLOR;
        let c = p.glow_color;
        let tint = vec3f(c[0] / d[0], c[1] / d[1], c[2] / d[2]);
        let lines = AIRGLOW_LINES.map(|(nm, rayleighs, km)| {
            let rgb = spectrum::xyz_to_rgb_keep_luminance(spectrum::line_xyz_per_rayleigh(nm)) * (rayleighs * strength);
            (rgb * tint, shell_k(km))
        });
        // The continuum makes up the rest of the default glow's zenith (G0), in its own colour.
        let g0 = luminance(vec3f(d[0], d[1], d[2])) * GLOW_SCALE;
        let lines_g0: f32 = AIRGLOW_LINES.iter().map(|&(nm, rayleighs, _)| spectrum::line_xyz_per_rayleigh(nm)[1] * rayleighs).sum();
        let continuum = CONTINUUM_TINT * tint * ((g0 - lines_g0).max(0.0) / luminance(CONTINUUM_TINT) * strength);
        let mut glow = Airglow { lines, continuum, zenith_t: extinction_at(extinction, 1.0), table: [Vec3f::default(); GLOW_ROWS] };
        if by_night {
            glow.table = std::array::from_fn(|i| {
                let u = i as f32 / (GLOW_ROWS - 1) as f32;
                glow.exact(u * u)
            });
        }
        glow
    }
}

/// What the moon's face (`moon_face` with Lommel-Seeliger share `ls_share`) is scaled by at phase angle `phase_deg` so
/// that the disc's whole light follows Allen's lunar phase law, its opposition surge included: Allen over the face's own
/// disc mean, 1 at full moon, about 0.37 at a quarter (where the face's mean is a quarter of a full moon's and the real
/// moon's light 0.09).
fn moon_phase_scale(phase_deg: f32, ls_share: f32) -> f32 {
    let a = phase_deg.abs().min(MOON_PHASE_MAX_DEG);
    let face = ls_share * lommel_seeliger_mean(a) + (1.0 - ls_share) * lambert_mean(a);
    allen(a) / face.max(1.0e-4)
}

/// Allen's lunar phase law (Astrophysical Quantities): the moon's whole light at phase angle `phase_deg` against a full
/// moon's, m(a) - m(0) = 0.026 |a| + 4e-9 a^4 (a in degrees).
fn allen(phase_deg: f32) -> f32 {
    let a = phase_deg.abs();
    10.0f32.powf(-0.4 * (0.026 * a + 4.0e-9 * a.powi(4)))
}

/// The disc's mean of the Lommel-Seeliger face 2 cos i / (cos i + cos e) at phase angle `phase_deg`, 1 at full moon:
/// its disc-integrated phase function 1 - sin(a/2) tan(a/2) ln(cot(a/4)).
fn lommel_seeliger_mean(phase_deg: f32) -> f32 {
    let r = phase_deg.abs().min(180.0).to_radians();
    if r < 1.0e-4 {
        return 1.0;
    }
    (1.0 - (0.5 * r).sin() * (0.5 * r).tan() * (1.0 / (0.25 * r).tan()).ln()).max(0.0)
}

/// The disc's mean of the Lambert face cos i at phase angle `phase_deg`: 2/3 of the Lambert sphere's phase function
/// (sin a + (pi - a) cos a) / pi, 2/3 at full moon. The same sum gives a Lambert sphere's light on what it faces per unit
/// of its albedo and of (radius / distance)^2: the Earth's on the moon (`earthshine`).
fn lambert_mean(phase_deg: f32) -> f32 {
    let a = phase_deg.abs().min(180.0).to_radians();
    (2.0 / 3.0 * (a.sin() + (PI - a) * a.cos()) / PI).max(0.0)
}

/// The earthshine on the moon's night side at the moon's phase angle `phase_deg`, against a full moon's disc (see
/// EARTH_ALBEDO): the Earth's light on the moon at its own phase, the supplement of the moon's, sent back as a full moon
/// sends the sun's (Allen's law at EARTHSHINE_PHASE_DEG). 5.4e-5 at new moon, 0 at full.
fn earthshine(phase_deg: f32) -> f32 {
    let r = EARTH_RADIUS_KM / MOON_DISTANCE_KM;
    EARTH_ALBEDO * lambert_mean(180.0 - phase_deg.abs().min(180.0)) * r * r * allen(EARTHSHINE_PHASE_DEG)
}

/// The face's Lommel-Seeliger share at phase angle `phase_deg` (see FACE_FIT_N): the least-squares fit of
/// a x Lommel-Seeliger + b x Lambert to the Hapke face (`hapke_face`) over the lit cells of a FACE_FIT_N x FACE_FIT_N
/// grid of the disc, a / (a + b), held to 0..1. About 0.1 ms.
fn lunar_lambert_share(phase_deg: f32) -> f32 {
    let (sin_g, cos_g) = phase_deg.abs().min(180.0).to_radians().sin_cos();
    // The viewer along +z, the sun in the x-z plane: a surface point (x, y, cos e) faces the sun by x sin g + cos e cos g.
    let (mut ss, mut sl, mut ll, mut hs, mut hl) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for j in 0..FACE_FIT_N {
        let y = (j as f32 + 0.5) / FACE_FIT_N as f32 * 2.0 - 1.0;
        for i in 0..FACE_FIT_N {
            let x = (i as f32 + 0.5) / FACE_FIT_N as f32 * 2.0 - 1.0;
            let r2 = x * x + y * y;
            if r2 >= 1.0 {
                continue;
            }
            let cos_e = (1.0 - r2).sqrt();
            let cos_i = x * sin_g + cos_e * cos_g;
            if !(cos_i > 0.0) {
                continue;
            }
            let h = hapke_face(cos_i, cos_e, cos_g) as f64;
            let (s, l) = ((2.0 * cos_i / (cos_i + cos_e)) as f64, cos_i as f64);
            ss += s * s;
            sl += s * l;
            ll += l * l;
            hs += h * s;
            hl += h * l;
        }
    }
    let det = ss * ll - sl * sl;
    if !(det > 0.0) {
        return 0.0;
    }
    let (a, b) = ((hs * ll - hl * sl) / det, (hl * ss - hs * sl) / det);
    let share = a / (a + b);
    if share.is_finite() { share.clamp(0.0, 1.0) as f32 } else { 0.0 }
}

/// Hapke's bidirectional reflectance of the moon's regolith (see HAPKE_W) at incidence and emission cosines `cos_i`,
/// `cos_e` (> 0) and the phase angle's cosine `cos_g`, without its constant w / 4 pi: the Lommel-Seeliger kernel of the
/// rough surface's effective cosines, times single scattering by the Henyey-Greenstein lobe raised by the shadow-hiding
/// surge plus isotropic multiple scattering (Chandrasekhar's H function, Hapke's (1 + 2x) / (1 + 2 sqrt(1 - w) x)), times
/// the mean slope's shadowing (`hapke_roughness`).
fn hapke_face(cos_i: f32, cos_e: f32, cos_g: f32) -> f32 {
    let (mu0, mu, shadow) = hapke_roughness(cos_i, cos_e, cos_g);
    let cos_g = cos_g.clamp(-1.0, 1.0);
    let xi = HAPKE_XI;
    let lobe = (1.0 - xi * xi) / (1.0 + 2.0 * xi * cos_g + xi * xi).powf(1.5);
    let surge = HAPKE_B0 / (1.0 + (0.5 * cos_g.acos()).min(1.5).tan() / HAPKE_H);
    let gamma = (1.0 - HAPKE_W).sqrt();
    let h = |x: f32| (1.0 + 2.0 * x) / (1.0 + 2.0 * gamma * x);
    mu0 / (mu0 + mu) * (lobe * (1.0 + surge) + h(mu0) * h(mu) - 1.0) * shadow
}

/// Hapke's (1984) macroscopic roughness for the mean slope HAPKE_SLOPE_DEG at `cos_i`, `cos_e` (> 0) and the phase
/// angle's cosine `cos_g`: the effective cosines of incidence and emission on the rough surface, and its shadowing
/// (1 at zero phase, where every shadow hides behind what casts it).
fn hapke_roughness(cos_i: f32, cos_e: f32, cos_g: f32) -> (f32, f32, f32) {
    let tan_t = HAPKE_SLOPE_DEG.to_radians().tan();
    let cot_t = 1.0 / tan_t;
    let chi = 1.0 / (1.0 + PI * tan_t * tan_t).sqrt();
    let (ci, ce) = (cos_i.clamp(1.0e-6, 1.0), cos_e.clamp(1.0e-6, 1.0));
    let (si, se) = ((1.0 - ci * ci).sqrt(), (1.0 - ce * ce).sqrt());
    // The azimuth psi between the planes of incidence and emission, and f(psi) = exp(-2 tan(psi / 2)).
    let cos_psi = if si * se > 1.0e-6 { ((cos_g - ci * ce) / (si * se)).clamp(-1.0, 1.0) } else { 1.0 };
    let psi = cos_psi.acos();
    let s2 = (0.5 * psi).sin().powi(2);
    let f = (-2.0 * (0.5 * psi).min(1.5).tan()).exp();
    // E1(x) = exp(-2/pi cot t cot x) and E2(x) = exp(-1/pi cot^2 t cot^2 x) for an angle x of cosine c and sine s, and
    // eta(x), the effective cosine of x alone.
    let cot_x = |c: f32, s: f32| c / s.max(1.0e-6);
    let e1 = |c: f32, s: f32| (-2.0 / PI * cot_t * cot_x(c, s)).exp();
    let e2 = |c: f32, s: f32| (-(cot_t * cot_x(c, s)).powi(2) / PI).exp();
    let eta = |c: f32, s: f32| chi * (c + s * tan_t * e2(c, s) / (2.0 - e1(c, s)));
    let (e1i, e2i, e1e, e2e) = (e1(ci, si), e2(ci, si), e1(ce, se), e2(ce, se));
    let (eta_i, eta_e) = (eta(ci, si), eta(ce, se));
    let k = psi / PI;
    if ci >= ce {
        // i <= e
        let d = 2.0 - e1e - k * e1i;
        let mu0 = chi * (ci + si * tan_t * (cos_psi * e2e + s2 * e2i) / d);
        let mu = chi * (ce + se * tan_t * (e2e - s2 * e2i) / d);
        (mu0, mu, mu / eta_e * ci / eta_i * chi / (1.0 - f + f * chi * ci / eta_i))
    } else {
        let d = 2.0 - e1i - k * e1e;
        let mu0 = chi * (ci + si * tan_t * (e2i - s2 * e2e) / d);
        let mu = chi * (ce + se * tan_t * (cos_psi * e2i + s2 * e2e) / d);
        (mu0, mu, mu / eta_e * ci / eta_i * chi / (1.0 - f + f * chi * ce / eta_e))
    }
}

/// The B-V colour index at quantile `q` of the naked-eye stars' (to magnitude 6.5), from the Bright Star
/// Catalogue's spread: the hot A and B stars round 0, the K giants round 1.1, few redder than 1.6.
fn bv_quantile(q: f32) -> f32 {
    // (B-V, share of the stars bluer than it)
    const CDF: [(f32, f32); 10] = [
        (-0.30, 0.0), (-0.10, 0.10), (0.10, 0.27), (0.30, 0.38), (0.50, 0.48),
        (0.80, 0.58), (1.00, 0.70), (1.20, 0.85), (1.50, 0.95), (1.90, 1.0),
    ];
    let q = q.clamp(0.0, 1.0);
    for w in CDF.windows(2) {
        let ((b0, c0), (b1, c1)) = (w[0], w[1]);
        if q <= c1 {
            return b0 + (b1 - b0) * (q - c0) / (c1 - c0);
        }
    }
    CDF[CDF.len() - 1].0
}

/// A star's temperature from its B-V index (Ballesteros 2012, the black-body fit): 4600 (1 / (0.92 B-V + 1.7) +
/// 1 / (0.92 B-V + 0.62)) kelvin.
fn bv_kelvin(bv: f32) -> f32 {
    4600.0 * (1.0 / (0.92 * bv + 1.7) + 1.0 / (0.92 * bv + 0.62))
}

impl TownDome {
    /// The dome of the town `lp` (as clamp() holds it) through the air `air`, lifted by `lift`, its rows built over
    /// `run` (about 1 M scattering steps in all: about a tenth of a second on one core); None when the runner skipped
    /// a row (an overtaken job's).
    fn new(lp: &LightPollutionParams, air: &AtmosphereParams, lift: f32, run: &dyn Fn(usize, &(dyn Fn(usize) + Sync))) -> Option<TownDome> {
        let air = air.clamped();
        let distance = lp.distance_km * 1000.0;
        let radius = (TOWN_RADIUS_M * lp.strength.sqrt()).clamp(TOWN_RADIUS_RANGE_M.0, TOWN_RADIUS_RANGE_M.1);
        let rows: Vec<std::sync::OnceLock<Vec<Vec3f>>> = (0..DOME_ROWS_N).map(|_| std::sync::OnceLock::new()).collect();
        run(DOME_ROWS_N, &|j| {
            let v = j as f32 / (DOME_ROWS_N - 1) as f32;
            let (sin_e, cos_e) = (v * v * FRAC_PI_2).sin_cos();
            let row = (0..DOME_COLUMNS_N)
                .map(|i| {
                    let u = i as f32 / (DOME_COLUMNS_N - 1) as f32;
                    let (sin_a, cos_a) = (u * u * PI).sin_cos();
                    town_scatter(vec3f(cos_e * cos_a, sin_e, cos_e * sin_a), distance, radius, air.air, air.haze)
                })
                .collect();
            let _ = rows[j].set(row);
        });
        let mut table = Vec::with_capacity(DOME_ROWS_N * DOME_COLUMNS_N);
        for row in rows {
            table.extend(row.into_inner()?.into_iter().map(|c| vec3f(c.x.max(1.0e-30).ln(), c.y.max(1.0e-30).ln(), c.z.max(1.0e-30).ln())));
        }
        // Walker's law at the reference town through a clear sky fixes the town's light.
        let natural = luminance(vec3f(DEFAULT_GLOW_COLOR[0], DEFAULT_GLOW_COLOR[1], DEFAULT_GLOW_COLOR[2])) * GLOW_SCALE;
        let at_45 = vec3f(FRAC_PI_4.cos(), FRAC_PI_4.sin(), 0.0);
        let reference = luminance(town_scatter(at_45, TOWN_REF_KM * 1000.0, TOWN_RADIUS_M, 1.0, 1.0));
        let walker = WALKER * TOWN_PEOPLE_PER_STRENGTH * TOWN_REF_KM.powf(-2.5) * natural;
        let light = lp.strength * walker / reference.max(1.0e-30);
        let lamp = |kelvin: f32| {
            let c = kelvin_to_rgb(kelvin);
            c / luminance(c).max(1.0e-9)
        };
        let (sodium, led) = (lamp(SODIUM_KELVIN), lamp(LED_KELVIN));
        let gain = (sodium + (led - sodium) * lp.lamps) * (light * lift);
        let (s, c) = lp.azimuth_deg.to_radians().sin_cos();
        // dir_from_az_el's frame: x east, -z north.
        Some(TownDome { toward: vec2f(s, -c), table, gain })
    }

    /// The town's glow along `dir`'s azimuth at elevation sine `y` (>= 0).
    fn at(&self, dir: Vec3f, y: f32) -> Vec3f {
        let horizontal = (dir.x * dir.x + dir.z * dir.z).sqrt();
        let elevation = y.atan2(horizontal);
        let cos_a = if horizontal > 1.0e-6 { (dir.x * self.toward.x + dir.z * self.toward.y) / horizontal } else { 1.0 };
        let away = cos_a.clamp(-1.0, 1.0).acos();
        let rv = (elevation.max(0.0) / FRAC_PI_2).min(1.0).sqrt() * (DOME_ROWS_N - 1) as f32;
        let cu = (away / PI).min(1.0).sqrt() * (DOME_COLUMNS_N - 1) as f32;
        let (j, i) = ((rv as usize).min(DOME_ROWS_N - 2), (cu as usize).min(DOME_COLUMNS_N - 2));
        let (fj, fi) = (rv - j as f32, cu - i as f32);
        let t = |j: usize, i: usize| self.table[j * DOME_COLUMNS_N + i];
        let low = t(j, i) + (t(j, i + 1) - t(j, i)) * fi;
        let high = t(j + 1, i) + (t(j + 1, i + 1) - t(j + 1, i)) * fi;
        let l = low + (high - low) * fj;
        vec3f(l.x.exp(), l.y.exp(), l.z.exp()) * self.gain
    }
}

/// A town's light scattered once toward the viewer along unit `view` (in a frame with the town `distance` metres
/// away along +x, lit over `radius` metres), per unit of its light, in the air of `air` and `haze` (the atmosphere's controls): Garstang's
/// (1986) single-scattering model on a curved Earth. The view ray is marched; at each step the town's upward light
/// (`TOWN_*`) reaches the air there through the air below, is scattered by the air (Rayleigh) and the haze (a
/// Henyey-Greenstein lobe), and comes back through the air in front.
fn town_scatter(view: Vec3f, distance: f32, radius: f32, air: f32, haze: f32) -> Vec3f {
    let earth = EARTH_RADIUS_KM * 1000.0;
    let rayleigh = DOME_RAYLEIGH * air;
    let mie = DOME_MIE_ANGSTROM * (DOME_MIE_550 * haze);
    // The town's ground lies below the viewer's tangent plane by d² / 2R; its up leans away from the viewer.
    let town = vec3f(distance, -DOME_VIEWER_M - distance * distance / (2.0 * earth), 0.0);
    let town_up = vec3f(distance / earth, 1.0, 0.0).normalize();
    let ring = radius * TOWN_RING;
    let lamps: [Vec3f; 7] = std::array::from_fn(|i| {
        if i == 0 {
            town
        } else {
            let (s, c) = (i as f32 * TAU / 6.0).sin_cos();
            town + vec3f(c * ring, 0.0, s * ring)
        }
    });
    let soft2 = (radius * TOWN_SOFT).powi(2);
    // Heights above the curved ground of a point in the viewer's frame (the viewer at its height).
    let height = |p: Vec3f| p.y + DOME_VIEWER_M + (p.x * p.x + p.z * p.z) / (2.0 * earth);
    // An exponential layer's optical depth along a straight path of `length` from the ground to height `h`.
    let column = |beta: Vec3f, scale: f32, h: f32, length: f32| {
        let h = h.max(1.0);
        beta * (length * scale * (1.0 - (-h / scale).exp()) / h)
    };
    // The ray leaves the scattering air (DOME_TOP_M over the curved ground) at `reach`: height(view s) = DOME_TOP_M
    // solved for s, in the form that keeps its precision straight up. It covers the town and all the air beyond it
    // that scatters, about 710 km out along a level ray.
    let flat = (view.x * view.x + view.z * view.z) / (2.0 * earth);
    let rise = DOME_TOP_M - DOME_VIEWER_M;
    let reach = 2.0 * rise / (view.y + (view.y * view.y + 4.0 * flat * rise).sqrt());
    // The town's light gathers where the ray passes it closest, `near` along the ray and `pass` from the town's centre,
    // and falls off about as the square of the distance from there: the steps are even in t for s = near + pass sinh(t),
    // short round the town and growing in proportion to the distance from it.
    let near = town.dot(view).clamp(0.0, reach);
    let pass = (town - view * near).length().max(1.0);
    let (t0, t1) = (-(near / pass).asinh(), ((reach - near) / pass).asinh());
    let step_end = |k: usize| (near + pass * (t0 + (t1 - t0) * (k as f32 / DOME_STEPS as f32)).sinh()).clamp(0.0, reach);
    let density = |h: f32| (h, (-h / DOME_RAYLEIGH_H).exp(), (-h / DOME_MIE_H).exp());
    let mut tau = Vec3f::default();
    let mut sum = Vec3f::default();
    let (mut s0, mut start) = (0.0, density(DOME_VIEWER_M));
    for k in 0..DOME_STEPS {
        let s1 = step_end(k + 1);
        let (s, ds) = (0.5 * (s0 + s1), s1 - s0);
        let p = view * s;
        let h = height(p);
        if h > DOME_TOP_M {
            break;
        }
        let (middle, end) = (density(h), density(height(view * s1)));
        let (_, n_r, n_m) = middle;
        // The air in front, to the step's middle: each half step's as the layers' exponentials averaged over its
        // heights, exact for a straight piece, so the long steps far from the town keep the air in front right.
        let piece = |a: (f32, f32, f32), b: (f32, f32, f32)| {
            (rayleigh * mean_density(a.1, b.1, a.0, b.0, DOME_RAYLEIGH_H) + mie * mean_density(a.2, b.2, a.0, b.0, DOME_MIE_H)) * (0.5 * ds)
        };
        tau += piece(start, middle);
        // The air below the step per metre of the path up from a lamp: `column` is linear in the length, and its
        // exponential is the step's alone, so it is taken once here and not for each lamp.
        let below_per_m = column(rayleigh, DOME_RAYLEIGH_H, h, 1.0) + column(mie, DOME_MIE_H, h, 1.0);
        for lamp in &lamps {
            let to = p - *lamp;
            let r2 = to.length_squared();
            let r = r2.sqrt();
            let up = to / r.max(1.0);
            let cos_psi = up.dot(town_up);
            if !(cos_psi > 0.0) {
                continue;
            }
            let psi = cos_psi.min(1.0).acos();
            let emitted = (2.0 * TOWN_GROUND_REFLECTANCE * (1.0 - TOWN_DIRECT_SHARE) * cos_psi + 0.554 * TOWN_DIRECT_SHARE * psi.powi(4))
                * smoothstep(0.0, TOWN_OBSTRUCTION_SIN, cos_psi);
            let below = below_per_m * r;
            let cos_theta = -up.dot(view);
            let phase_r = 3.0 / (16.0 * PI) * (1.0 + cos_theta * cos_theta);
            let phase_m = henyey_greenstein(cos_theta, DOME_MIE_G);
            let scatter = rayleigh * (n_r * phase_r) + mie * (n_m * DOME_MIE_ALBEDO * phase_m);
            let through = vec3f((-(tau.x + below.x)).exp(), (-(tau.y + below.y)).exp(), (-(tau.z + below.z)).exp());
            sum += scatter * through * (emitted / (7.0 * (r2 + soft2)) * ds);
        }
        tau += piece(middle, end);
        (s0, start) = (s1, end);
    }
    sum
}

/// The mean density of an exponential layer of scale height `scale` over a straight piece from height `a` to height
/// `b`, its densities there `na` and `nb`: (na - nb) scale / (b - a), or their mean where the heights are too close
/// for the difference.
fn mean_density(na: f32, nb: f32, a: f32, b: f32, scale: f32) -> f32 {
    let dh = b - a;
    if dh.abs() < 0.01 * scale { 0.5 * (na + nb) } else { (na - nb) * scale / dh }
}

/// The Henyey-Greenstein phase function, normalised over the sphere.
fn henyey_greenstein(cos_g: f32, g: f32) -> f32 {
    let d = (1.0 + g * g - 2.0 * g * cos_g).max(1.0e-6);
    (1.0 - g * g) / (4.0 * PI * d * d.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{dir_from_az_el, vec, AuroraParams, Env, HdriParams, MilkyWayParams, NebulaParams};
    use makepad_render_material::ibl;

    #[test]
    fn star_hours_reads_solar_time_or_the_manual_sun() {
        // The default sun: time mode, longitude 0, time zone 0.
        let mut sun = SunParams { hour: 22.0, ..Default::default() };
        assert!((star_hours(&sun) - 22.0).abs() < 1.0e-5);
        // 15 degrees east of the zone's meridian, the sun runs an hour ahead of the clock.
        sun.longitude = 15.0;
        assert!((star_hours(&sun) - 23.0).abs() < 1.0e-5);
        sun.hour = 23.5;
        assert!((star_hours(&sun) - 0.5).abs() < 1.0e-4, "wraps past midnight");
        // Manual: the hour is read off the sun's azimuth. East is 6 h, the noon side 12 h, west 18 h.
        sun.mode = "manual".to_string();
        sun.latitude = 45.0;
        for (az, hours) in [(90.0, 6.0), (180.0, 12.0), (270.0, 18.0)] {
            sun.azimuth_deg = az;
            assert!((star_hours(&sun) - hours).abs() < 1.0e-4, "north, az {az}");
        }
        sun.latitude = -30.0;
        for (az, hours) in [(90.0, 6.0), (0.0, 12.0), (270.0, 18.0)] {
            sun.azimuth_deg = az;
            assert!((star_hours(&sun) - hours).abs() < 1.0e-4, "south, az {az}");
        }
    }

    /// The direction at the centre of an equirect texel, in ibl's convention.
    fn uv_dir(u: f32, v: f32) -> Vec3f {
        vec(ibl::equirect_uv_to_dir([u, v]))
    }

    fn night_at(p: &NightParams, sun_elevation_deg: f32) -> NightSky {
        NightSky::new(p, 7, dir_from_az_el(200.0, sun_elevation_deg), 22.0, 45.0)
    }

    /// Every grid cell that can touch the unit sphere.
    fn all_cells() -> impl Iterator<Item = [i32; 3]> {
        let n = STAR_GRID as i32 + 1;
        (-n..=n).flat_map(move |z| (-n..=n).flat_map(move |y| (-n..=n).map(move |x| [x, y, z])))
    }

    /// The first star, in grid order, whose world direction is at least `min_y` up.
    fn first_star_above(sky: &NightSky, min_y: f32) -> Option<Vec3f> {
        all_cells().filter_map(|c| sky.star_in_cell(c)).map(|s| sky.to_world(s.dir)).find(|w| w.y > min_y)
    }

    /// The luminance of the moon's face's albedo along `dir`, as `NightSky::moon` reads it there (its finest level).
    fn face_albedo(sky: &NightSky, dir: Vec3f) -> f32 {
        let m = sky.moon.as_ref().expect("a moon");
        let p = project(&m.frame, dir).expect("on the moon");
        let a = moon_face::albedo_rgb(m.face.sample(-p.x / m.tan_radius, p.y / m.tan_radius, 0.0));
        luminance(vec3f(a[0], a[1], a[2]))
    }

    /// A direction on the moon's disc at the screen coordinates (sx to the right,
    /// sy up) of a viewer facing it, in units of the radius. ibl's basis puts the
    /// frame's `right` on the viewer's left, hence the minus.
    fn moon_point(frame: &LightFrame, radius_deg: f32, sx: f32, sy: f32) -> Vec3f {
        let t = radius_deg.to_radians().tan();
        (frame.center - frame.right * (sx * t) + frame.up * (sy * t)).normalize()
    }

    #[test]
    fn night_factor_follows_the_sun() {
        let p = NightParams::default();
        assert_eq!(night_at(&p, 10.0).night_factor(), 0.0);
        assert_eq!(night_at(&p, 0.0).night_factor(), 0.0);
        assert!((night_at(&p, -6.0).night_factor() - 0.5).abs() < 1.0e-3);
        assert_eq!(night_at(&p, -13.0).night_factor(), 1.0);
    }

    #[test]
    fn stars_and_glow_are_zero_by_day() {
        let day = night_at(&NightParams::default(), 60.0);
        for i in 0..800 {
            let dir = uv_dir(((i % 40) as f32 + 0.5) / 40.0, ((i / 40) as f32 + 0.5) / 20.0);
            assert_eq!(day.stars(dir), Vec3f::default());
            assert_eq!(day.glow(dir), Vec3f::default());
            assert_eq!(day.ground(dir, Vec3f::all(0.2)), Vec3f::default());
        }
        assert_eq!(day.ambient(), Vec3f::default());
    }

    #[test]
    fn the_default_field_holds_thousands_of_stars_evenly() {
        let night = night_at(&NightParams::default(), -30.0);
        let stars: Vec<Vec3f> = all_cells().filter_map(|c| night.star_in_cell(c)).map(|s| s.dir).collect();
        // About 4,700 at density 0.5, the Milky Way boost included (the naked-eye sky
        // holds about 9,000 to magnitude 6.5).
        assert!(stars.len() > 2500 && stars.len() < 7000, "{} stars", stars.len());
        for s in &stars {
            assert!((s.length() - 1.0).abs() < 1.0e-5, "a star is a unit direction: {s:?}");
        }
        // No pinch at the poles: the polar caps (|y| > 0.8, a fifth of the sphere) hold
        // about a fifth of the stars.
        let polar = stars.iter().filter(|s| s.y.abs() > 0.8).count() as f32 / stars.len() as f32;
        assert!(polar > 0.12 && polar < 0.28, "polar share {polar}");
    }

    #[test]
    fn more_stars_along_the_milky_way() {
        let night = night_at(&NightParams::default(), -30.0);
        // Two bands of equal solid angle: within 10 degrees of the galactic equator
        // (|sin b| < 0.17) and a band far from it (0.5 < |sin b| <= 0.67).
        let (mut band, mut away) = (0usize, 0usize);
        for s in all_cells().filter_map(|c| night.star_in_cell(c)) {
            let sin_b = night.gal_pole.dot(s.dir).abs();
            if sin_b < 0.17 {
                band += 1;
            } else if sin_b > 0.5 && sin_b <= 0.67 {
                away += 1;
            }
        }
        assert!(away > 200 && band as f32 > 1.3 * away as f32, "milky way {band}, away {away}");
    }

    #[test]
    fn stars_shine_at_night_with_finite_energy() {
        let night = night_at(&NightParams::default(), -30.0);
        // A star the field placed high in the sky is lit at its centre.
        let star = first_star_above(&night, 0.3).expect("the default density places stars");
        let at_star = night.stars(star);
        assert!(at_star.is_finite() && luminance(at_star) > 0.0, "{at_star:?}");
        // Total starlight over the upper hemisphere. It must be finite and positive, and
        // far below a full moon's (adapted) irradiance of about 0.04.
        let (w, h) = (256usize, 128usize);
        let mut total = 0.0f64;
        for y in 0..h / 2 {
            let theta = (y as f32 + 0.5) / h as f32 * PI;
            let d_omega = (TAU / w as f32) * (PI / h as f32) * theta.sin();
            for x in 0..w {
                let l = night.stars(uv_dir((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32));
                assert!(l.is_finite() && l.x >= 0.0 && l.y >= 0.0 && l.z >= 0.0);
                total += (luminance(l) * d_omega) as f64;
            }
        }
        assert!(total > 1.0e-8 && total < 1.0e-3, "starlight {total}");
    }

    #[test]
    fn the_star_profile_integrates_to_one() {
        // Riemann sum of the windowed two-Gaussian profile over its tangent plane.
        let step = STAR_SIGMA_CORE / 6.0;
        let n = (3.0 * STAR_SIGMA_HALO / step).ceil() as i32;
        let mut sum = 0.0f64;
        for i in -n..=n {
            for j in -n..=n {
                let (x, y) = (i as f32 * step, j as f32 * step);
                sum += star_kernel(x * x + y * y) as f64;
            }
        }
        let mass = sum * (step as f64) * (step as f64);
        assert!((mass - 1.0).abs() < 0.02, "{mass}");
    }

    #[test]
    fn a_star_is_never_clipped_by_its_cell() {
        // Every direction within a star's reach gathers that star: the grid, the
        // shell and the reach are chosen so the star's cell is always among the
        // 3x3x3 cells around the direction's own.
        assert!(STAR_GRID * (3.0 * STAR_SIGMA_HALO).asin() + STAR_SHELL < 1.0);
        let night = night_at(&NightParams::default(), -30.0);
        let star = all_cells().filter_map(|c| night.star_in_cell(c)).next().expect("a star");
        // Sample a ring at 0.8 of the reach around the star, in star space.
        let axis = if star.dir.y.abs() < 0.9 { vec3f(0.0, 1.0, 0.0) } else { vec3f(1.0, 0.0, 0.0) };
        let t1 = Vec3f::cross(axis, star.dir).normalize();
        let t2 = Vec3f::cross(star.dir, t1);
        let r = 0.8 * (3.0 * STAR_SIGMA_HALO);
        let expected = star.color * (star.flux * star_kernel(r * r));
        for i in 0..24 {
            let a = i as f32 / 24.0 * TAU;
            let s = (star.dir * (1.0 - r * r).sqrt() + t1 * (r * a.cos()) + t2 * (r * a.sin())).normalize();
            let got = night.star_field(s);
            // Other stars may add to it, never take from it.
            assert!(got.x >= expected.x * 0.999 && got.y >= expected.y * 0.999 && got.z >= expected.z * 0.999, "{got:?} < {expected:?}");
        }
    }

    #[test]
    fn a_512_preview_shows_stars_above_the_glow() {
        let p = NightParams { moon: false, ..Default::default() };
        let night = night_at(&p, -30.0);
        let (w, h) = (512usize, 256usize);
        let mut visible = 0;
        for y in 0..h / 2 {
            for x in 0..w {
                let dir = uv_dir((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
                if luminance(night.stars(dir)) > luminance(night.glow(dir)) {
                    visible += 1;
                }
            }
        }
        assert!(visible >= 10, "only {visible} star pixels stand out at 512x256");
    }

    #[test]
    fn the_seed_places_the_stars() {
        let p = NightParams::default();
        let a = NightSky::new(&p, 7, dir_from_az_el(200.0, -30.0), 22.0, 45.0);
        let b = NightSky::new(&p, 7, dir_from_az_el(200.0, -30.0), 22.0, 45.0);
        let c = NightSky::new(&p, 8, dir_from_az_el(200.0, -30.0), 22.0, 45.0);
        let star = first_star_above(&a, 0.3).expect("a star");
        assert_eq!(a.stars(star), b.stars(star));
        let moved = all_cells().any(|cell| a.star_in_cell(cell) != c.star_in_cell(cell));
        assert!(moved, "another seed gives another sky");
    }

    #[test]
    fn the_moons_lit_side_faces_the_sun() {
        // The default moon: az 135, el 30, 0.52 degrees across.
        let p = NightParams::default();
        let frame = light_frame(135.0, 30.0, 0.0);
        let r = 0.5 * p.moon_size_deg;
        // The sun below the horizon, 90 degrees of azimuth clockwise from the moon,
        // i.e. to the viewer's right: lit on the right.
        let right_lit = NightSky::new(&p, 1, dir_from_az_el(225.0, -10.0), 22.0, 45.0);
        let right = luminance(right_lit.moon(moon_point(&frame, r, 0.6, 0.0)));
        let left = luminance(right_lit.moon(moon_point(&frame, r, -0.6, 0.0)));
        assert!(right > 100.0 * left, "right {right}, left {left}");
        assert!(left > 0.0, "earthshine keeps the dark side faintly visible");
        // The sun on the other side: lit on the left.
        let left_lit = NightSky::new(&p, 1, dir_from_az_el(45.0, -10.0), 22.0, 45.0);
        let right = luminance(left_lit.moon(moon_point(&frame, r, 0.6, 0.0)));
        let left = luminance(left_lit.moon(moon_point(&frame, r, -0.6, 0.0)));
        assert!(left > 100.0 * right, "right {right}, left {left}");
        // The sun opposite the moon: a full moon, lit evenly (each point's light over its face's albedo there).
        let full = NightSky::new(&p, 1, dir_from_az_el(315.0, -30.0), 22.0, 45.0);
        let a = luminance(full.moon(moon_point(&frame, r, 0.6, 0.0))) / face_albedo(&full, moon_point(&frame, r, 0.6, 0.0));
        let b = luminance(full.moon(moon_point(&frame, r, -0.6, 0.0))) / face_albedo(&full, moon_point(&frame, r, -0.6, 0.0));
        assert!(a > 0.0 && (a - b).abs() < 0.01 * a, "{a} {b}");
        // Outside the disc there is no moon.
        assert_eq!(full.moon(dir_from_az_el(135.0, 31.0)), Vec3f::default());
    }

    #[test]
    fn the_moon_is_a_key_only_at_night_and_when_risen() {
        let key = |p: &NightParams, sun_el: f32| NightSky::new(p, 1, dir_from_az_el(315.0, sun_el), 22.0, 45.0).moon_key();
        let mut p = NightParams::default();
        assert!(key(&p, 20.0).is_none(), "day");
        assert!(key(&p, -3.0).is_none(), "civil twilight still outshines the moon");
        let k = key(&p, -10.0).expect("night, moon up");
        assert!(k.dir.dot(dir_from_az_el(135.0, 30.0)) > 0.99999);
        assert!(k.radiance.is_finite() && luminance(k.radiance) > 0.0);
        assert!((k.cos_radius - 0.26f32.to_radians().cos()).abs() < 1.0e-6);
        // A hard-edged disc under a degree wide: all its emission reaches a facing
        // surface, and it is its own covering cone.
        assert_eq!((k.facing, k.cos_cover), (1.0, k.cos_radius));
        assert!(k.validate().is_ok(), "{k:?}");
        p.moon_elevation_deg = -5.0;
        assert!(key(&p, -10.0).is_none(), "a set moon is no key");
        p.moon_elevation_deg = 30.0;
        p.moon = false;
        assert!(key(&p, -10.0).is_none(), "no moon, no key");
    }

    /// M6: the moon's key carries the disc's whole emission (its mean over
    /// the disc's true solid angle) through the f32 cone it stores, at the
    /// 0.1 degree minimum as at the real moon's half degree.
    #[test]
    fn the_moons_key_carries_its_emission_through_its_f32_cone() {
        for size_deg in [0.1f32, 0.53] {
            let p = NightParams { moon_size_deg: size_deg, ..Default::default() };
            let night = NightSky::new(&p, 1, dir_from_az_el(315.0, -30.0), 22.0, 45.0);
            let key = night.moon_key().expect("night, moon up");
            let m = night.moon.as_ref().unwrap();
            let r = (0.5 * size_deg).to_radians();
            let want = m.mean * (4.0 * PI * (0.5 * r).sin().powi(2));
            let got = key.irradiance();
            for (g, w) in [(got.x, want.x), (got.y, want.y), (got.z, want.z)] {
                assert!((g / w - 1.0).abs() < 1.0e-6, "{size_deg} deg: {g} for {w}");
            }
        }
    }

    /// The natural glow brightens toward the horizon (van Rhijn, less the air's extinction over the shells' own air
    /// mass: Krisciunas & Schaefer), and is greener and redder there than at the zenith (the lines' shells gain more
    /// than the even continuum); the air dims it more with more haze. Before the lines it was one colour 5.7 times
    /// the zenith at the horizon; dark-sky photographs show about 1.1 to 1.5.
    #[test]
    fn night_glow_brightens_toward_the_horizon() {
        let p = NightParams { moon: false, ..Default::default() };
        let night = night_at(&p, -30.0);
        let zenith = night.glow(vec3f(0.0, 1.0, 0.0));
        let low: Vec<Vec3f> = [1.0f32, 5.0, 10.0].iter().map(|&el| night.glow(dir_from_az_el(0.0, el))).collect();
        println!("glow over the zenith's: {:?}", low.iter().map(|c| luminance(*c) / luminance(zenith)).collect::<Vec<_>>());
        for c in &low {
            assert!(luminance(*c) > 1.1 * luminance(zenith) && luminance(*c) < 3.0 * luminance(zenith), "zenith {zenith:?}, low {c:?}");
            assert!(c.z / c.y < zenith.z / zenith.y, "bluest at the zenith: {zenith:?}, low {c:?}");
        }
        assert_eq!(night.glow(vec3f(0.0, -1.0, 0.0)), Vec3f::default());
        // Haze dims the glow near the horizon against the zenith.
        let sun = dir_from_az_el(200.0, -30.0);
        let hazy_air = AtmosphereParams { haze: 4.0, ..AtmosphereParams::default() };
        let hazy = NightSky::new_par(&p, 7, &Atmosphere::new(sun, &hazy_air, &SunDiscParams::default()), &hazy_air, 22.0, 45.0, &super::super::caster::serial_rows).unwrap();
        let ratio = |n: &NightSky| luminance(n.glow(dir_from_az_el(0.0, 5.0))) / luminance(n.glow(vec3f(0.0, 1.0, 0.0)));
        assert!(ratio(&hazy) < ratio(&night), "hazy {}, clear {}", ratio(&hazy), ratio(&night));
    }

    #[test]
    fn moonlight_reaches_the_ground_and_the_clouds() {
        let p = NightParams::default();
        let moonlit = NightSky::new(&p, 1, dir_from_az_el(315.0, -30.0), 22.0, 45.0);
        let mut dark = p.clone();
        dark.moon = false;
        let moonless = NightSky::new(&dark, 1, dir_from_az_el(315.0, -30.0), 22.0, 45.0);
        let grey = Vec3f::all(0.2);
        let down = vec3f(0.0, -1.0, 0.0);
        let lit = luminance(moonlit.ground(down, grey));
        let unlit = luminance(moonless.ground(down, grey));
        assert!(unlit > 0.0 && lit > 5.0 * unlit, "moonlit {lit}, moonless {unlit}");
        assert!(luminance(moonlit.ambient()) > luminance(moonless.ambient()));
        assert!(luminance(moonless.ambient()) > 0.0);
        assert_eq!(moonlit.ground(vec3f(0.0, 1.0, 0.0), grey), Vec3f::default());
    }

    #[test]
    fn a_night_env_draws_the_moon_and_makes_it_the_key() {
        let mut p = HdriParams::default();
        p.sky.sun.mode = "manual".to_string();
        p.sky.sun.elevation_deg = -30.0;
        // Opposite the default moon (az 135, el 30): a full moon.
        p.sky.sun.azimuth_deg = 315.0;
        let env = Env::new(&p);
        let moon_dir = dir_from_az_el(135.0, 30.0);
        let at_moon = luminance(env.radiance(moon_dir));
        let beside = luminance(env.radiance(dir_from_az_el(135.0, 36.0)));
        assert!(at_moon > 100.0 * beside, "moon {at_moon}, sky beside it {beside}");
        let key = env.sun().expect("with the sun 30 degrees down, the risen moon is the key");
        assert!(key.dir.dot(moon_dir) > 0.99999);
        assert!(key.radiance.is_finite() && luminance(key.radiance) > 0.0);
        // The map's yaw turns the moon and its key together, counter-clockwise seen
        // from above (ibl's sign): azimuth 135 shows at 95.
        p.rotation_deg = 40.0;
        let turned = Env::new(&p);
        let turned_dir = dir_from_az_el(95.0, 30.0);
        assert!(turned.sun().expect("still the key").dir.dot(turned_dir) > 0.99999);
        assert!(luminance(turned.radiance(turned_dir)) > 100.0 * beside);
        let map = turned.bake(32);
        assert!(map.data.iter().all(|t| t.iter().all(|v| v.is_finite() && *v >= 0.0)));
    }

    /// Plan hdri-7 §5 (physical scale): a surface brightness as engine luminance on the stars' own anchor: the default
    /// glow's zenith is 21.89 mag/arcsec² (G0, 3.0e-8), 2.5 magnitudes are 10x, and one unit is about 6300 cd/m².
    #[test]
    fn surface_brightness_is_on_the_stars_anchor() {
        let g0 = luminance(vec3f(0.02, 0.03, 0.06)) * GLOW_SCALE;
        assert!((mag_arcsec2_to_radiance(21.89) / g0 - 1.0).abs() < 0.01, "{} vs {g0}", mag_arcsec2_to_radiance(21.89));
        assert!((mag_arcsec2_to_radiance(20.0) / mag_arcsec2_to_radiance(22.5) - 10.0).abs() < 1.0e-3);
        assert!((cd_m2_per_engine_unit() / 6303.0 - 1.0).abs() < 0.005, "{}", cd_m2_per_engine_unit());
    }

    /// A moonless night (the sun 30 degrees down at azimuth 200, 22 h, 45 N) with the Milky Way as `mw` sets it.
    fn milky_night(mw: MilkyWayParams) -> NightSky {
        night_at(&NightParams { moon: false, milky_way: Some(mw), ..Default::default() }, -30.0)
    }

    /// Star space at galactic longitude `l` and latitude `b` (degrees), by the sky's own galactic axes.
    fn galactic(sky: &NightSky, l: f32, b: f32) -> Vec3f {
        let (l, b) = (l.to_radians(), b.to_radians());
        let [c, pole, l90] = sky.gal_axes;
        (c * (b.cos() * l.cos()) + l90 * (b.cos() * l.sin()) + pole * b.sin()).normalize()
    }

    /// The band's luminance at (l, b) over the dark sky's zenith glow (G0 at the same lift), extinction aside.
    fn band_in_g0(sky: &NightSky, l: f32, b: f32) -> f32 {
        // The glow's zenith above the air: the band is, too.
        let zenith = luminance(sky.glow(vec3f(0.0, 1.0, 0.0)) / sky.transmittance(1.0));
        luminance(sky.milky_way.as_ref().unwrap().at(galactic(sky, l, b), 0.0).0) / zenith
    }

    /// Plan hdri-7 §2, §5 (refs §5.2): at Strength 1 and Core 1, as the eye sees it (Realism 1; at 0 a long exposure
    /// shows it MW_PHOTO_GAIN times brighter, Task 3b), the band is the real sky's, in units of the dark sky's
    /// zenith (G0, 21.9 mag/arcsec²): the core region (|l| <= 30, |b| <= 10) 4.4 +- 1 with its dust; without dust the
    /// mid longitudes 1.5 to 2.5, the anticentre 0.7 to 1.5, 0.1 to 0.45 at 30 degrees out, at most 0.15 at the poles,
    /// never over about 9 (the star clouds are capped with the band: MW_PEAK_MU's range, 19.5 to 19.6, is 9.04 G0 at
    /// most); the Great Rift (b +1, l 10..60) lets 0.25 to 0.45 through; the core is warmer than the anticentre. With
    /// Task 3b's constants (fix round 1) it prints about: core region 3.59, mid 1.89, anticentre 1.07, 30 deg 0.20,
    /// poles 0.02, peak 8.50; rift 0.41.
    #[test]
    fn the_milky_way_is_the_real_skys() {
        let clean = milky_night_at(MilkyWayParams { dust: 0.0, ..MilkyWayParams::default() }, 1.0);
        let mean = |sky: &NightSky, ls: std::ops::RangeInclusive<i32>, bs: &[f32]| {
            let (mut sum, mut n) = (0.0f32, 0.0f32);
            for l in ls {
                for &b in bs {
                    sum += band_in_g0(sky, l as f32, b);
                    n += 1.0;
                }
            }
            sum / n
        };
        let mid = 0.5 * (mean(&clean, 80..=100, &[0.0]) + mean(&clean, 260..=280, &[0.0]));
        let anti = mean(&clean, 170..=190, &[0.0]);
        let thirty = mean(&clean, -20..=20, &[30.0, -30.0]);
        let poles = band_in_g0(&clean, 0.0, 90.0).max(band_in_g0(&clean, 0.0, -90.0));
        let peak = (-30..=30).flat_map(|l| (-10..=10).map(move |b| (l, b))).map(|(l, b)| band_in_g0(&clean, l as f32, b as f32)).fold(0.0, f32::max);
        let dusty = milky_night_at(MilkyWayParams::default(), 1.0);
        let bs: Vec<f32> = (-10..=10).map(|b| b as f32).collect();
        let core = mean(&dusty, -30..=30, &bs);
        let rift = (10..=60).map(|l| dusty.milky_way.as_ref().unwrap().at(galactic(&dusty, l as f32, 1.0), 0.0).1).sum::<f32>() / 51.0;
        println!("G0 units: core region {core:.2}, mid {mid:.2}, anticentre {anti:.2}, 30 deg {thirty:.2}, poles {poles:.3}, peak {peak:.2}; rift transmittance {rift:.2}");
        assert!((core - 4.4).abs() <= 1.0, "the core region {core}");
        assert!((1.5..=2.5).contains(&mid), "mid longitudes {mid}");
        assert!((0.7..=1.5).contains(&anti), "the anticentre {anti}");
        assert!((0.1..=0.45).contains(&thirty), "30 degrees out {thirty}");
        assert!(poles <= 0.15, "the poles {poles}");
        assert!(peak <= 9.04 * 1.001, "the brightest patch {peak}");
        assert!((0.25..=0.45).contains(&rift), "the rift lets {rift} through");
        let red_over_blue = |l: f32| {
            let c = clean.milky_way.as_ref().unwrap().at(galactic(&clean, l, 0.0), 0.0).0;
            c.x / c.z
        };
        assert!(red_over_blue(0.0) > red_over_blue(180.0), "warmer at the core");
    }

    /// The band's ridge is the galactic equator: round the whole band its light at b 0 is well over its light 6 degrees
    /// either side.
    #[test]
    fn the_band_lies_along_the_galactic_equator() {
        let sky = milky_night(MilkyWayParams { dust: 0.0, ..MilkyWayParams::default() });
        let around = |b: f32| (0..72).map(|i| band_in_g0(&sky, i as f32 * 5.0, b)).sum::<f32>();
        let (on, above, below) = (around(0.0), around(6.0), around(-6.0));
        assert!(on > 1.4 * above && on > 1.4 * below, "{on} against {above} and {below}");
    }

    /// Plan hdri-7 D3: Turn and Tilt turn the whole night sphere. The band lives in star space, so its axes there are the
    /// star sphere's whatever Turn and Tilt say; in the map's frame the galactic centre the feature reports and a star both
    /// move, by the same turn: the angle between them is the unturned one. (The stars' thickening along the band is the
    /// star field's own boost in star space, pinned by `more_stars_along_the_milky_way`; it turns with the rows.)
    #[test]
    fn turn_and_tilt_turn_the_stars_and_the_band_as_one() {
        let at = |turn: f32, tilt: f32| milky_night(MilkyWayParams { turn_deg: turn, tilt_deg: tilt, ..MilkyWayParams::default() });
        let base = at(0.0, 0.0);
        let star = all_cells().filter_map(|c| base.star_in_cell(c)).next().expect("a star").dir;
        let base_centre = base.features().galactic_centre.expect("the band draws");
        let base_star = base.to_world(star);
        for (turn, tilt) in [(116.0, 0.0), (40.0, 25.0), (300.0, -60.0)] {
            let sky = at(turn, tilt);
            assert_eq!(sky.gal_axes, base.gal_axes, "turn {turn}, tilt {tilt}: the band moved in star space");
            let (centre, seen) = (sky.features().galactic_centre.expect("the band draws"), sky.to_world(star));
            assert!((centre - sky.to_world(sky.gal_axes[0])).length() < 1.0e-6, "turn {turn}, tilt {tilt}: the feature is the turned centre");
            assert!(seen.dot(base_star) < 0.9999, "turn {turn}, tilt {tilt}: the star stayed");
            assert!(centre.dot(base_centre) < 0.9999, "turn {turn}, tilt {tilt}: the band's centre stayed");
            assert!((seen.dot(centre) - base_star.dot(base_centre)).abs() < 1.0e-5, "turn {turn}, tilt {tilt}: the star and the band part");
        }
        let plain = night_at(&NightParams { moon: false, ..Default::default() }, -30.0);
        assert_eq!(base.rows, plain.rows, "Turn 0 and Tilt 0 are the plain rows, bit for bit");
    }

    /// Plan hdri-7 §2: the band shows only in astronomical twilight and darker: none with the sun 12 degrees down (the
    /// stars already full), some at 15, all of it from 18; none by day.
    #[test]
    fn the_milky_way_waits_for_astronomical_twilight() {
        assert_eq!(ASTRO_FADE_DEG, (12.0, 18.0));
        let band = |el: f32| {
            let sky = night_at(&NightParams { moon: false, milky_way: Some(MilkyWayParams::default()), ..Default::default() }, el);
            luminance(sky.milky_way.as_ref().unwrap().at(galactic(&sky, 40.0, 3.0), 0.0).0)
        };
        assert_eq!(band(10.0), 0.0);
        assert_eq!(band(-12.0), 0.0);
        assert!(band(-15.0) > 0.0 && band(-15.0) < band(-18.0));
        assert!((band(-18.0) - band(-40.0)).abs() <= 1.0e-6 * band(-40.0), "full from -18");
    }

    /// Plan hdri-7 §2: the moon washes the band out as a real sky does, through the light it puts in the air (no factor of
    /// its own): a full moon takes the band's contrast over the sky down more than five times, and more near the moon (its
    /// contrast there keeps a smaller share of the moonless one than far from it: the two points' band differs, so each
    /// is measured against its own moonless contrast); and in proportion to the moon's brightness: the sky the moon adds
    /// (1 / contrast less the moonless one) is three times as large at Moon brightness 1.5 as at 0.5 (the moon's sky light
    /// is linear in it; the night's lift cancels in a contrast).
    #[test]
    fn the_moon_washes_the_milky_way_out() {
        let mw = MilkyWayParams { dust: 0.0, ..MilkyWayParams::default() };
        let dark = milky_night(mw.clone());
        // band points well up in this sky, the moon 8 degrees above the first
        let up: Vec<Vec3f> = (0..36).map(|i| dark.to_world(galactic(&dark, i as f32 * 10.0, 3.0))).filter(|w| w.y > 0.3).collect();
        let (near, far) = (up[0], *up.iter().min_by(|a, b| a.dot(up[0]).total_cmp(&b.dot(up[0]))).unwrap());
        let (az, el) = crate::az_el_from_dir(near);
        let p = NightParams { moon: true, moon_azimuth_deg: az, moon_elevation_deg: (el + 8.0).min(85.0), milky_way: Some(mw), ..Default::default() };
        let lit_at = |brightness: f32| NightSky::new(&NightParams { moon_brightness: brightness, ..p.clone() }, 7, dir_from_az_el(az + 180.0, -30.0), 22.0, 45.0);
        let lit = NightSky::new(&p, 7, dir_from_az_el(az + 180.0, -30.0), 22.0, 45.0);
        let contrast = |sky: &NightSky, w: Vec3f| luminance(sky.milky_way.as_ref().unwrap().at(sky.to_star(w), 0.0).0) / luminance(sky.glow(w));
        let (c_dark, c_lit) = (contrast(&dark, near), contrast(&lit, near));
        let (c_far_dark, c_far) = (contrast(&dark, far), contrast(&lit, far));
        println!("contrast near the moon: moonless {c_dark:.3}, full moon {c_lit:.4}; far from it: moonless {c_far_dark:.3}, full moon {c_far:.4}");
        assert!(c_dark > 5.0 * c_lit, "the moon takes {c_dark} only to {c_lit}");
        assert!(c_lit / c_dark < c_far / c_far_dark, "more near the moon: x{} of the moonless contrast, against x{} far from it", c_lit / c_dark, c_far / c_far_dark);
        let (c_half, c_bright) = (contrast(&lit_at(0.5), near), contrast(&lit_at(1.5), near));
        let share = (1.0 / c_bright - 1.0 / c_dark) / (1.0 / c_half - 1.0 / c_dark);
        println!("contrast near the moon at Moon brightness 0.5 {c_half:.4}, 1.5 {c_bright:.4}: the moon's sky x{share:.3}");
        assert!(c_bright < c_half && c_half < c_dark, "a brighter moon washes it out more: {c_dark}, {c_half}, {c_bright}");
        assert!((share - 3.0).abs() < 0.03, "in proportion to the moon's brightness: x{share}, not x3");
    }

    /// The Milky Way's dust dims the stars behind it, and only while the band draws; off, `deep_sky` is `stars` bit for bit.
    /// A star dims by the dust along its lookup at full detail, alike at every width.
    #[test]
    fn the_dust_dims_the_stars_behind_it() {
        let off = night_at(&NightParams { moon: false, ..Default::default() }, -30.0);
        let clear = milky_night(MilkyWayParams { turn_deg: 0.0, strength: 0.0, dust: 0.0, ..MilkyWayParams::default() });
        let dusty = milky_night(MilkyWayParams { turn_deg: 0.0, strength: 0.0, dust: 2.0, ..MilkyWayParams::default() });
        let (mut dimmed, mut seen) = (0, 0);
        for c in all_cells() {
            let Some(star) = off.star_in_cell(c) else { continue };
            let w = off.to_world(star.dir);
            if !(w.y > 0.2) {
                continue;
            }
            assert_eq!(off.deep_sky(w, 0.0), off.stars(w), "no band: deep_sky is stars");
            assert_eq!(clear.deep_sky(w, 0.0), off.stars(w), "no dust, no band light, the same rows: the same stars");
            let t = dusty.milky_way.as_ref().unwrap().at(star.dir, 0.0).1;
            // a star is a point: a held bake's texel (512 wide) dims it as an exact lookup does
            assert_eq!(dusty.deep_sky(w, std::f32::consts::TAU / 512.0), dusty.deep_sky(w, 0.0), "a star dims alike at every width");
            if t < 0.6 {
                seen += 1;
                if luminance(dusty.deep_sky(w, 0.0)) < 0.7 * luminance(off.stars(w)) {
                    dimmed += 1;
                }
            }
        }
        assert!(seen > 5 && dimmed == seen, "{dimmed} of {seen} stars behind the dust dimmed");
    }

    /// A moonless night with the Milky Way as `mw` sets it, the deep sky at `realism` (the nebulas' block, off: it sets
    /// the look and draws nothing).
    fn milky_night_at(mw: MilkyWayParams, realism: f32) -> NightSky {
        night_at(&NightParams { moon: false, milky_way: Some(mw), nebulas: Some(NebulaParams { on: false, realism, ..NebulaParams::default() }), ..Default::default() }, -30.0)
    }

    /// Two unit vectors across `w`.
    fn across(w: Vec3f) -> (Vec3f, Vec3f) {
        let up = if w.y.abs() < 0.9 { vec3f(0.0, 1.0, 0.0) } else { vec3f(1.0, 0.0, 0.0) };
        let t1 = Vec3f::cross(up, w).normalize();
        (t1, Vec3f::cross(w, t1))
    }

    /// `n` directions `radius` (radians) round `w`.
    fn ring(w: Vec3f, radius: f32, n: usize) -> Vec<Vec3f> {
        let (t1, t2) = across(w);
        (0..n).map(|i| {
            let a = i as f32 * std::f32::consts::TAU / n as f32;
            (w * radius.cos() + (t1 * a.cos() + t2 * a.sin()) * radius.sin()).normalize()
        }).collect()
    }

    /// Task 3b: the core is the band's brightest part. At Turn 116 (the galactic centre about 16 degrees up in the south)
    /// the band as the map shows it (its extinction in, the stars off), each point read as its mean over a 3-degree disc,
    /// is brightest near the centre: the brightest point within 6 degrees of it beats every point of the band (|b| up to
    /// 10 degrees, above the horizon) 60 degrees or more from it by 15 % at least.
    #[test]
    fn the_core_is_the_bands_brightest_part() {
        let sky = night_at(&NightParams { moon: false, star_brightness: 0.0, milky_way: Some(MilkyWayParams::default()), ..Default::default() }, -30.0);
        let centre = sky.features().galactic_centre.expect("the band draws");
        assert!(centre.y > 0.15, "the premise: the centre is up ({centre:?})");
        let texel = std::f32::consts::TAU / 1024.0;
        let disc = |w: Vec3f| {
            let mut looks = vec![w];
            looks.extend(ring(w, 1.5f32.to_radians(), 8));
            looks.extend(ring(w, 3.0f32.to_radians(), 16));
            looks.iter().map(|d| luminance(sky.deep_sky(*d, texel))).sum::<f32>() / looks.len() as f32
        };
        let (mut near, mut far, mut far_at) = (0.0f32, 0.0f32, (0, 0));
        for l in -4..=4 {
            for b in -4..=4 {
                near = near.max(disc(sky.to_world(galactic(&sky, 1.5 * l as f32, 1.5 * b as f32))));
            }
        }
        for l in (0..360).step_by(2) {
            for b in (-10..=10).step_by(2) {
                let s = galactic(&sky, l as f32, b as f32);
                let w = sky.to_world(s);
                if s.dot(sky.gal_axes[0]) <= 0.5 && w.y > 0.05 {
                    let v = disc(w);
                    if v > far {
                        (far, far_at) = (v, (l, b));
                    }
                }
            }
        }
        println!("the band near the galactic centre {near:.3e}, 60 degrees or more from it at most {far:.3e} (l {}, b {}): x{:.2}", far_at.0, far_at.1, near / far);
        assert!(far > 0.0, "the premise: the band shows far from the centre");
        assert!(near > 1.15 * far, "the core {near} against {far} at l {}, b {}", far_at.0, far_at.1);
    }

    /// Task 3b: at Realism 0 (as photographed) the band runs from warm to cool: its core region (|l| and |b| up to 10
    /// and 5 degrees) is redder (R/B) than the band 90 degrees along it, either way, by a third at least; at Realism 1
    /// (as the eye sees it) the band is near grey wherever it shows (saturation at most 0.1).
    #[test]
    fn the_bands_colour_runs_from_warm_to_cool() {
        let saturation = |c: Vec3f| {
            let (hi, lo) = (c.x.max(c.y).max(c.z), c.x.min(c.y).min(c.z));
            if hi > 0.0 { (hi - lo) / hi } else { 0.0 }
        };
        let mean = |sky: &NightSky, ls: std::ops::RangeInclusive<i32>| {
            let mut sum = Vec3f::default();
            for l in ls {
                for b in -5..=5 {
                    sum += sky.milky_way.as_ref().unwrap().at(galactic(sky, l as f32, b as f32), 0.0).0;
                }
            }
            sum
        };
        let art = milky_night_at(MilkyWayParams::default(), 0.0);
        let red_over_blue = |c: Vec3f| c.x / c.z;
        let core = red_over_blue(mean(&art, -10..=10));
        for arm in [80..=100, 260..=280] {
            let along = red_over_blue(mean(&art, arm.clone()));
            println!("Realism 0: R/B in the core region {core:.3}, at l {arm:?} {along:.3}");
            assert!(core > 1.33 * along, "the core's R/B {core} against {along} at l {arm:?}");
        }
        let eye = milky_night_at(MilkyWayParams::default(), 1.0);
        let mut most = 0.0f32;
        for l in (0..360).step_by(5) {
            for b in (-10..=10).step_by(5) {
                let c = eye.milky_way.as_ref().unwrap().at(galactic(&eye, l as f32, b as f32), 0.0).0;
                most = most.max(saturation(c));
                assert!(saturation(c) <= 0.1, "Realism 1 shows {c:?} at l {l}, b {b}");
            }
        }
        println!("Realism 1: the band's saturation at most {most:.3}");
    }

    /// Task 3b: the dust has structure at many scales. Over the plane near the core (lines along l from -30 to 30 degrees
    /// at b -4 to 4, and across b from -12 to 12 at l -20 to 20), its darkness (1 - transmittance, at texel 0, Dust 1)
    /// has power at 0.5, 2 and 8 degrees: the RMS of a box of each width less a box of twice it, each over a floor.
    #[test]
    fn the_dust_has_structure_at_many_scales() {
        let sky = milky_night(MilkyWayParams::default());
        let m = sky.milky_way.as_ref().unwrap();
        let step = 0.05f32;
        let mut lines: Vec<Vec<f32>> = Vec::new();
        for b in [-4.0f32, -2.0, 0.0, 2.0, 4.0] {
            lines.push((0..=1200).map(|i| 1.0 - m.transmittance(galactic(&sky, -30.0 + step * i as f32, b), 0.0)).collect());
        }
        for l in [-20.0f32, -10.0, 0.0, 10.0, 20.0] {
            lines.push((0..=480).map(|i| 1.0 - m.transmittance(galactic(&sky, l, -12.0 + step * i as f32), 0.0)).collect());
        }
        let boxed = |v: &[f32], i: usize, half: usize| v[i - half..=i + half].iter().sum::<f32>() / (2 * half + 1) as f32;
        for (width, floor) in [(0.5f32, 0.01f32), (2.0, 0.02), (8.0, 0.03)] {
            let (half, wide) = ((0.5 * width / step).round() as usize, (width / step).round() as usize);
            let (mut sum, mut n) = (0.0f32, 0usize);
            for line in &lines {
                for i in wide..line.len() - wide {
                    let d = boxed(line, i, half) - boxed(line, i, wide);
                    sum += d * d;
                    n += 1;
                }
            }
            let rms = (sum / n as f32).sqrt();
            println!("the dust's darkness at {width} degrees: RMS {rms:.4} (floor {floor})");
            assert!(rms > floor, "no dark features {width} degrees wide: RMS {rms}");
        }
    }

    /// Task 3b: the two Magellanic Clouds sit south of the plane, at l 280.5, b -32.9 and l 302.8, b -44.3: the band's
    /// light at each is over three times its mean on a ring round it (12 and 8 degrees out). They turn with the stars:
    /// Turn and Tilt move a cloud and a star as one, and with the Large one turned up, the map shows it over the sky
    /// round it.
    #[test]
    fn the_magellanic_clouds_sit_south_of_the_plane() {
        let sky = milky_night(MilkyWayParams::default());
        let m = sky.milky_way.as_ref().unwrap();
        let band = |s: Vec3f| luminance(m.at(s, 0.0).0);
        for (l, b, out) in [(280.5f32, -32.9f32, 12.0f32), (302.8, -44.3, 8.0)] {
            let s = galactic(&sky, l, b);
            let round = ring(s, out.to_radians(), 24).iter().map(|d| band(*d)).sum::<f32>() / 24.0;
            println!("the band at l {l}, b {b}: {:.3e} against {round:.3e} {out} degrees round it", band(s));
            assert!(band(s) > 3.0 * round, "l {l}, b {b}: {} against {round}", band(s));
        }
        let at = |turn: f32, tilt: f32| milky_night(MilkyWayParams { turn_deg: turn, tilt_deg: tilt, ..MilkyWayParams::default() });
        let base = at(0.0, 0.0);
        let star = all_cells().filter_map(|c| base.star_in_cell(c)).next().expect("a star").dir;
        let lmc = galactic(&base, 280.5, -32.9);
        let mut shown = false;
        for turn in (0..360).step_by(20) {
            let sky = at(turn as f32, -60.0);
            let w = sky.to_world(lmc);
            assert!((sky.to_world(star).dot(w) - base.to_world(star).dot(base.to_world(lmc))).abs() < 1.0e-5, "turn {turn}: the cloud and the star part");
            if w.y > 0.5 && !shown {
                shown = true;
                let texel = std::f32::consts::TAU / 1024.0;
                let dark = night_at(&NightParams { moon: false, star_brightness: 0.0, milky_way: Some(MilkyWayParams { turn_deg: turn as f32, tilt_deg: -60.0, ..MilkyWayParams::default() }), ..Default::default() }, -30.0);
                let round = ring(w, 12.0f32.to_radians(), 24).iter().map(|d| luminance(dark.deep_sky(*d, texel))).sum::<f32>() / 24.0;
                let here = luminance(dark.deep_sky(w, texel));
                println!("turn {turn}, tilt -60: the Large Magellanic Cloud {:.0} degrees up, {here:.3e} against {round:.3e} round it", w.y.asin().to_degrees());
                assert!(here > 2.0 * round, "turned up, the cloud shows {here} against {round}");
            }
        }
        assert!(shown, "the premise: a turn shows the Large Magellanic Cloud well up");
    }

    /// Plan hdri-7 §3: the nebulas fade with daylight as the Milky Way does: none with the sun 12 degrees down, some at
    /// 15, all of them from 18.
    #[test]
    fn the_nebulas_wait_for_astronomical_twilight() {
        let gas = |el: f32| {
            let sky = night_at(&NightParams { moon: false, nebulas: Some(NebulaParams { amount: 1.0, ..NebulaParams::default() }), ..Default::default() }, el);
            let n = sky.nebulas.as_ref().unwrap();
            n.centres().iter().map(|(c, _)| luminance(n.at(*c, 0.0).0)).sum::<f32>()
        };
        assert_eq!(gas(-12.0), 0.0);
        assert!(gas(-15.0) > 0.0 && gas(-15.0) < gas(-18.0));
        assert!((gas(-18.0) - gas(-40.0)).abs() <= 1.0e-5 * gas(-40.0), "full from -18");
    }

    /// Plan hdri-7 §3 ("dark dust in front"): a dark cloud's dust dims what lies behind it, the band and the stars, by its
    /// own transmittance. At the centre of a dark cloud well up that no other cloud reaches, with the Milky Way on,
    /// `deep_sky` is what it is without the nebulas times the cloud's `through` (`Nebulas::at`'s second value), and less.
    #[test]
    fn the_nebulas_dust_dims_what_lies_behind() {
        let mw = MilkyWayParams::default();
        let off = night_at(&NightParams { moon: false, milky_way: Some(mw.clone()), ..Default::default() }, -30.0);
        for seed in 1..=64u32 {
            let neb = NebulaParams { amount: 1.0, seed, ..NebulaParams::default() };
            let on = night_at(&NightParams { moon: false, milky_way: Some(mw.clone()), nebulas: Some(neb), ..Default::default() }, -30.0);
            let n = on.nebulas.as_ref().unwrap();
            let clouds = n.centres();
            let alone = |i: usize| clouds.iter().enumerate().all(|(j, (c, r))| j == i || c.dot(clouds[i].0) < (1.05 * (r + clouds[i].1)).min(3.1).cos());
            let dark = (0..clouds.len()).find(|&i| {
                let (gas, through) = n.at(clouds[i].0, 0.0);
                gas == Vec3f::default() && through < 0.8 && on.to_world(clouds[i].0).y > 0.2 && alone(i)
            });
            let Some(i) = dark else { continue };
            let w = on.to_world(clouds[i].0);
            let through = n.at(on.to_star(w), 0.0).1;
            let (dimmed, clear) = (on.deep_sky(w, 0.0), off.deep_sky(w, 0.0));
            println!("seed {seed}: a dark cloud lets {through:.3} through: {} against {}", luminance(dimmed), luminance(clear));
            assert!(luminance(clear) > 0.0, "the premise: the band and the stars behind it");
            assert!((dimmed - clear * through).length() <= 1.0e-4 * clear.length(), "{dimmed:?} against {clear:?} x {through}");
            assert!(luminance(dimmed) < luminance(clear));
            assert!((on.star_transmittance(w) - off.star_transmittance(w) * through).abs() <= 1.0e-6, "the stars dim by both dusts: {} against {} x {through}", on.star_transmittance(w), off.star_transmittance(w));
            return;
        }
        panic!("the premise: a dark cloud well up, alone, among 64 seeds");
    }

    /// Plan hdri-7 §3 ("they fade with ... the moon like the Milky Way"): through the light the moon puts in the air, a
    /// full moon 8 degrees above a glowing cloud takes its contrast over the sky down more than five times, and more near
    /// the moon than far from it (the Milky Way's test, `the_moon_washes_the_milky_way_out`, on two nebulas): the far cloud
    /// stands more than 60 degrees off and at least as high, so its moonless glow is no brighter, and a moon that lit the
    /// sky evenly would wash it out at least as much as the near one.
    #[test]
    fn the_moon_washes_the_nebulas_out() {
        for seed in 1..=32u32 {
            let neb = NebulaParams { amount: 1.0, seed, ..NebulaParams::default() };
            let dark = night_at(&NightParams { moon: false, nebulas: Some(neb.clone()), ..Default::default() }, -30.0);
            let n = dark.nebulas.as_ref().unwrap();
            let glowing: Vec<Vec3f> = n.centres().into_iter().map(|(c, _)| dark.to_world(c)).filter(|w| luminance(n.at(dark.to_star(*w), 0.0).0) > 0.0 && w.y > 0.3).collect();
            let pair = glowing.iter().filter(|w| w.y < 0.8).find_map(|&near| {
                let far = glowing.iter().filter(|f| f.y >= near.y && f.dot(near) < 0.5).min_by(|a, b| a.dot(near).total_cmp(&b.dot(near)))?;
                Some((near, *far))
            });
            let Some((near, far)) = pair else { continue };
            let (az, el) = crate::az_el_from_dir(near);
            let p = NightParams { moon: true, moon_azimuth_deg: az, moon_elevation_deg: (el + 8.0).min(85.0), nebulas: Some(neb), ..Default::default() };
            let lit = NightSky::new(&p, 7, dir_from_az_el(az + 180.0, -30.0), 22.0, 45.0);
            let contrast = |sky: &NightSky, w: Vec3f| luminance(sky.nebulas.as_ref().unwrap().at(sky.to_star(w), 0.0).0) / luminance(sky.glow(w));
            let (c_dark, c_lit) = (contrast(&dark, near), contrast(&lit, near));
            let (c_far_dark, c_far) = (contrast(&dark, far), contrast(&lit, far));
            println!(
                "seed {seed}: a nebula's contrast near the moon: moonless {c_dark:.3}, full moon {c_lit:.4}; {:.0} degrees off: moonless {c_far_dark:.3}, full moon {c_far:.4}",
                far.dot(near).clamp(-1.0, 1.0).acos().to_degrees()
            );
            assert!(c_dark > 5.0 * c_lit, "the moon takes {c_dark} only to {c_lit}");
            assert!(c_lit / c_dark < c_far / c_far_dark, "more near the moon: x{} of the moonless contrast, against x{} far from it", c_lit / c_dark, c_far / c_far_dark);
            return;
        }
        panic!("the premise: a glowing cloud well up, and a second one far off and as high, among 32 seeds");
    }

    /// Plan hdri-7 §4: the aurora fades with the night as the stars do (night factor x lift): none by day or at
    /// sunset, at -6 degrees (32 x 0.5) / 1024 of its night strength.
    #[test]
    fn the_aurora_follows_the_night_factor() {
        let p = NightParams { moon: false, aurora: Some(AuroraParams::default()), ..Default::default() };
        assert!(!night_at(&p, 5.0).has_aurora() && !night_at(&p, 0.0).has_aurora());
        let d = dir_from_az_el(0.0, 18.0);
        let (dusk, night) = (luminance(night_at(&p, -6.0).aurora(d, 0.0)), luminance(night_at(&p, -20.0).aurora(d, 0.0)));
        assert!(night > 0.0, "the premise: the arc shows 18 degrees up in the north");
        assert!((dusk / night - 16.0 / 1024.0).abs() < 1.0e-4, "{dusk} / {night}");
        assert_eq!(night_at(&p, -20.0).aurora(dir_from_az_el(0.0, -5.0), 0.0), Vec3f::default(), "none below the horizon");
    }

    /// Plan hdri-7 D9: the aurora lights the scene: the night's ground and ambient gain its light, green; a great storm
    /// (IBC IV, Strength 20) over about a steradian puts 0.03 to 0.3 of a full moon's light on the ground (refs §5.1;
    /// the sheet's face-on body is 0.3 of its brightest lines of sight, EDGE_MAX).
    #[test]
    fn the_aurora_lights_the_ground_green() {
        let dark = night_at(&NightParams { moon: false, ..Default::default() }, -30.0);
        let lit = night_at(&NightParams { moon: false, aurora: Some(AuroraParams::default()), ..Default::default() }, -30.0);
        let gain = lit.ground_irradiance - dark.ground_irradiance;
        assert!(gain.y > gain.x && gain.y > gain.z && gain.y > 0.0, "{gain:?}");
        assert!(luminance(lit.ambient) > luminance(dark.ambient));
        let storm = AuroraParams { strength: 20.0, width_deg: 180.0, height_deg: 30.0, ..AuroraParams::default() };
        let great = night_at(&NightParams { moon: false, aurora: Some(storm), ..Default::default() }, -30.0);
        let moon = NightSky::new(&NightParams { moon_elevation_deg: 45.0, moon_azimuth_deg: 135.0, ..Default::default() }, 7, dir_from_az_el(315.0, -30.0), 22.0, 45.0);
        let ratio = luminance(great.ground_irradiance - dark.ground_irradiance) / luminance(moon.moon_ground);
        println!("a Strength 20 storm puts {ratio:.3} of a full moon's light on the ground");
        assert!((0.03..=0.3).contains(&ratio), "{ratio}");
    }

    /// Plan hdri-7 §4: the moon does not hide the aurora but lowers its contrast: under a full moon (the moonlit
    /// preset's 1.5) its brightest point still adds over 3 % to the sky there, and less than on a moonless night.
    #[test]
    fn the_moon_lowers_the_auroras_contrast_but_does_not_hide_it() {
        let p = |moon: bool| NightParams { moon, moon_elevation_deg: 30.0, moon_azimuth_deg: 105.0, moon_brightness: 1.5, glow_strength: 0.8, aurora: Some(AuroraParams::default()), ..Default::default() };
        let contrast = |sky: &NightSky| {
            let mut most = 0.0f32;
            for beta in -45..=45 {
                for e in 4..80 {
                    let d = dir_from_az_el(beta as f32, e as f32 * 0.5);
                    most = most.max(luminance(sky.aurora(d, 0.0)) / luminance(sky.glow(d)));
                }
            }
            most
        };
        let sun = dir_from_az_el(285.0, -30.0);
        let (lit, dark) = (contrast(&NightSky::new(&p(true), 7, sun, 19.0, 45.0)), contrast(&NightSky::new(&p(false), 7, sun, 19.0, 45.0)));
        println!("the aurora's best contrast: {dark:.2} moonless, {lit:.3} under the moon");
        assert!(lit > 0.03 && lit < dark, "{lit} under the moon, {dark} without");
    }
}
