//! Clear-sky atmosphere for the HDRI generator.
//!
//! Scattering by air (Rayleigh) and haze (Mie, two Cornette-Shanks lobes; a
//! low layer the Haze control sets and the stratosphere's thin one), with
//! ozone absorption, on a spherical Earth, in eight spectral bands taken
//! to RGB at the end. The marching scheme follows wwwtyro/glsl-atmosphere
//! (Unlicense, https://github.com/wwwtyro/glsl-atmosphere): march the view ray
//! through the shell and, at every sample, take the transmittance toward the
//! sun. What an environment map needs beyond that shader is added here:
//! ozone, the Earth's shadow (so twilight is right), a tabulated transmittance
//! toward the sun in place of the nested march, every order of scattering
//! after the first (Hillaire 2020, with the ground's bounce), a lit ground
//! below the horizon and an energy-preserving sun disc.
//!
//! ## Calibration
//!
//! The haze (its depth, phase and colour) is fitted against real clear-sky
//! HDRIs at sun heights from 1 to 85 deg, compared at the same sun height:
//! the sky's radiance on rings from the zenith down to 3 deg and against the
//! angle from the sun, its colour, and the sky's light on level ground
//! against the sun's. A clear noon's sky puts about 0.13 of the sun's light
//! on level ground (real ones 0.10-0.15), the glow round the sun falls as
//! real ones do, a sunset's zenith stays blue, and the stratosphere's haze
//! keeps the glow over the sunrise point bright in civil twilight.
//!
//! A clear sky mirrors about the sun's vertical plane, so everything that
//! depends on the view is computed once into a sky-view table indexed by the
//! view elevation and the azimuth away from the sun. The table keeps the air's,
//! the low haze's, the stratosphere's and the multiple scattering apart and
//! without the phase, which is the same all along a view ray; `sky()` reads the
//! four planes bilinearly and applies the exact phase for its own direction (the
//! low haze takes its kind's, the stratosphere's haze always the continental
//! one), so the haze's narrow peak round the sun stays sharp however coarse the
//! table.
//!
//! Units: lengths in metres, tabulated in f64 so planet-sized numbers keep
//! their precision; radiance in the scene units fixed by `SUN_IRRADIANCE`.
//! Directions are in the map's own frame (before the map rotation): y up,
//! x east, -z north, the frame `ibl::dir_to_equirect_uv` maps.
//!
//! ## Radiance scale
//!
//! Everything is linear Rec.709 radiance in the units `SUN_IRRADIANCE` sets.
//! With 20.0 per channel at the top of the atmosphere, a high sun delivers a
//! ground irradiance of about 16.5 in luminance ((17.4, 16.5, 14.4) rgb), so
//! a white wall facing it reads E / pi = 5.3 plus a few tenths from the sky,
//! level ground gets 2.2 from the sky on top of the sun's 16.5, a clear zenith
//! 30 deg from the sun sits at about half a unit and the 0.53 deg disc at
//! about 3e5.
//! Against the engine, per the spec's Conventions:
//!
//! - `ibl::EnvPreset` (render_material/src/ibl.rs:285-381) paints surfaces at
//!   0.1-1, softboxes at 2-9 and `Sunset`'s 0.6 deg disc at 300, which carries
//!   only ~0.1 of irradiance: those are looks, not a photometric scale. The
//!   generator keeps the sun-to-sky ratio real (a thousand times the preset's
//!   disc) so `ibl::prefilter` and `ibl::sh9` see a key light that agrees with
//!   `Env::sun`.
//! - The renderer's HDR lane (`SunLight::to_hdr`, sun.rs:103-117): the direct
//!   term is 0.72 x 3.0 = 2.16 and the shaders apply it as `color * N.L` with
//!   no 1/pi, so "a noon white wall sits near 2.0 before exposure"; the day
//!   fill is 0.28 x 0.8 = 0.22 per hemisphere; the analytic dome is drawn at
//!   its Preetham luminance x `HDR_SKY_GAIN` = 0.4 (renderer/frame.rs:9), and
//!   `SunLight::hdr_exposure` meters 0.75 over the rig's key, clamped to
//!   0.25..3.2.
//!
//! The generator's outdoor scale is therefore about 3x the engine's HDR lane
//! (a noon wall of 6 against 2). Nothing here rescales: `Ibl.intensity`
//! (0.35 reproduces the stock brightness) or phase 2's environment-metered
//! exposure absorbs the factor, and `tests::the_documented_scale_holds` pins
//! these numbers so this note cannot rot.

use makepad_draw::*;
use super::{dir_from_az_el, AtmosphereParams, EnvSun, SunDiscParams, SunMode, SunParams};
use crate::sky::{noaa_solar_position, SkyDate};

/// The sky from a viewer at a height of its own (`AtmosphereParams::viewer_height_m`).
mod aerial;
pub use aerial::AirLight;

/// Top-of-atmosphere sun irradiance, in scene units per channel. The scale is
/// arbitrary but fixed (see the module header for how it compares with the
/// engine's presets and HDR lane): with it a clear high-sun zenith lands at a
/// luminance of about half a unit, and sun, sky, clouds and ground keep their
/// physical ratios to one another.
pub const SUN_IRRADIANCE: f32 = 20.0;

const PLANET_RADIUS: f64 = 6_360_000.0;
const TOP_RADIUS: f64 = 6_420_000.0;
/// Above the dense air (TOP_RADIUS, 60 km) the mesosphere goes on to 100 km. Its air is too thin to dim anything
/// (0.05 % of the air lies above 60 km), so the sun's light reaches it whole unless its ray dips back into the dense
/// air (`DepthTable::transmittance_at`), but it still scatters: once the sun is about 7 degrees down, the air still
/// lit overhead and the nearest lit air toward the sun are up there, and their light, scattered once to the viewer
/// and again on its way down, keeps the sky glowing until the sun is 18 degrees down. Without it the sky went black
/// between 12 and 15 degrees down. The view march and the multiple scattering's rays cross it in MESOSPHERE_STEPS
/// steps after the dense air's.
const MESOSPHERE_RADIUS: f64 = 6_460_000.0;
const MESOSPHERE_STEPS: usize = 6;
/// The mesosphere's air is the real one (US Standard Atmosphere 1976: 2.53e-4 of the ground's density at 60 km,
/// 1.51e-5 at 80 km, 2.8e-6 at 90 km), not the dense air's exponential carried on, which the calibration holds below
/// 60 km and which is already twice the real density there and eight times it at 100 km.
const MESOSPHERE_DENSITY: f64 = 2.5e-4;
const MESOSPHERE_SCALE_HEIGHT: f64 = 7_000.0;
/// The viewer stands 200 m up, so a level view ray clears the ground.
const VIEWER_HEIGHT: f64 = 200.0;
const RAYLEIGH_SCALE_HEIGHT: f64 = 8_000.0;
const MIE_SCALE_HEIGHT: f64 = 1_200.0;
/// The light is carried in BANDS spectral bands, 40 nm wide and centred on
/// BAND_NM, and taken to RGB at the end (`Medium::rgb`). Three channels at
/// 680, 550 and 440 nm (Bruneton's and Hillaire's real-time choice) cannot
/// hold a low sun: the red channel's 680 nm crosses the air 2.6x more easily
/// than the light a camera's red sees (around 600 nm), and it misses ozone's
/// Chappuis band, which peaks at 600 nm and is what keeps a twilight zenith
/// blue (Hulburt 1953). With them, a 1 deg sun's zenith and a dawn's sky
/// came out grey-purple where real HDRIs are blue.
pub(crate) const BANDS: usize = 8;
type Spectrum = [f64; BANDS];
pub(crate) const BAND_NM: [f64; BANDS] = [400.0, 440.0, 480.0, 520.0, 560.0, 600.0, 640.0, 680.0];
/// CIE 1931 2 deg colour-matching functions (x, y, z), 380..700 nm every 10 nm.
const CIE_XYZ: [[f64; 3]; 33] = [
    [0.001368, 0.000039, 0.006450], [0.004243, 0.000120, 0.020050], [0.014310, 0.000396, 0.067850],
    [0.043510, 0.001210, 0.207400], [0.134380, 0.004000, 0.645600], [0.283900, 0.011600, 1.385600],
    [0.348280, 0.023000, 1.747060], [0.336200, 0.038000, 1.772110], [0.290800, 0.060000, 1.669200],
    [0.195360, 0.090980, 1.287640], [0.095640, 0.139020, 0.812950], [0.032010, 0.208020, 0.465180],
    [0.004900, 0.323000, 0.272000], [0.009300, 0.503000, 0.158200], [0.063270, 0.710000, 0.078250],
    [0.165500, 0.862000, 0.042160], [0.290400, 0.954000, 0.020300], [0.433450, 0.994950, 0.008750],
    [0.594500, 0.995000, 0.003900], [0.762100, 0.952000, 0.002100], [0.916300, 0.870000, 0.001650],
    [1.026300, 0.757000, 0.001100], [1.062200, 0.631000, 0.000800], [1.002600, 0.503000, 0.000340],
    [0.854450, 0.381000, 0.000190], [0.642400, 0.265000, 0.000050], [0.447900, 0.175000, 0.000020],
    [0.283500, 0.107000, 0.000000], [0.164900, 0.061000, 0.000000], [0.087400, 0.032000, 0.000000],
    [0.046770, 0.017000, 0.000000], [0.022700, 0.008210, 0.000000], [0.011359, 0.004102, 0.000000],
];
/// The sun's spectral irradiance above the air, W m^-2 nm^-1, 380..700 nm
/// every 10 nm (the spectrum Bruneton's precomputed-scattering demo uses).
/// Only its shape matters: the bands are scaled so that it is white.
const SOLAR_SPECTRUM: [f64; 33] = [
    1.01249, 1.14716, 1.72765, 1.73054, 1.6887, 1.61253, 1.91198, 2.03474, 2.02042, 2.02212, 1.93377,
    1.95809, 1.91686, 1.8298, 1.8685, 1.8931, 1.85149, 1.8504, 1.8341, 1.8345, 1.8147, 1.78158, 1.7533,
    1.6965, 1.68194, 1.64654, 1.6048, 1.52143, 1.55622, 1.5113, 1.474, 1.4482, 1.41018,
];
/// Haze 1, "a clear day": the aerosol's vertical optical depth at 550 nm.
/// Measured clear days sit at 0.05-0.15 (continental sites; a visibility of
/// 40-70 km). This one is fitted against real clear-sky HDRIs at sun heights
/// from 1 to 85 deg: the sky's light on level ground against the sun's, and
/// the glow round the sun. The scale height puts its sea-level extinction at
/// depth / MIE_SCALE_HEIGHT.
const AEROSOL_DEPTH: f64 = 0.07;
/// The aerosol's extinction falls with wavelength as lambda^-1.3 (the
/// Angstrom exponent of continental haze), so it is a little bluer than white.
const AEROSOL_ANGSTROM: f64 = 1.3;
/// Haze absorbs a little as well: it scatters 90 % of what it takes out.
const AEROSOL_ALBEDO: f64 = 0.9;
/// Haze scatters mostly forward, with a narrow peak round the sun (its
/// diffraction) over a broad lobe: the sum of two Cornette-Shanks phases,
/// the peak carrying MIE_PEAK_SHARE of the light. One lobe cannot do both:
/// a single g of 0.76 lit the sky 6-15 deg from the sun 1.4x too bright
/// against real HDRIs while it lit the first 5 deg too dim. Together their
/// mean cosine is 0.78, a continental haze's.
const MIE_G_BROAD: f64 = 0.65;
/// The stratosphere's own thin aerosol (the Junge layer, 15-25 km), there on
/// any day whatever the haze down low: a vertical optical depth of 0.008 at
/// 550 nm, a tent peaking at STRAT_PEAK_HEIGHT. Lit by a sun that has set for
/// the air below it, it scatters forward into the bright glow over the
/// sunrise or sunset point and the purple light of civil twilight. Its
/// sulphate droplets keep the continental haze's colour, absorption and phase
/// whatever the low haze's kind (`Aerosol`, which is the low haze's alone).
const STRAT_DEPTH: f64 = 0.008;
const STRAT_PEAK_HEIGHT: f64 = 20_000.0;
const STRAT_HALF_WIDTH: f64 = 7_000.0;
const MIE_G_PEAK: f64 = 0.95;
const MIE_PEAK_SHARE: f64 = 0.25;
/// The boundary layer (`AtmosphereParams::haze_top_m`): the haze mixed evenly from the ground up to the layer's
/// top (the mixing height) and, over the capping inversion, the free troposphere's thin haze: FREE_HAZE_SHARE of the
/// layer's density just over the top, falling with FREE_HAZE_SCALE_HEIGHT (lidar profiles over land: a tenth or less
/// over the inversion, a scale height of 2-3 km above it). The Haze control keeps its column (its optical depth), so a
/// low layer holds the same haze denser near the ground.
const FREE_HAZE_SHARE: f64 = 0.1;
const FREE_HAZE_SCALE_HEIGHT: f64 = 2_500.0;
/// The sun's angular radius (0.2666 deg, its mean): seen from a point of the air, the planet's limb hides its disc
/// gradually, so the Earth's shadow on the air has a penumbra (`DepthTable::transmittance_at`). The drawn disc's size
/// is a look and leaves the shadow alone.
const SUN_ANGULAR_RADIUS: f64 = 0.004_653;
/// The disc's light across its face for the penumbra: a linear limb darkening with the visible band's mean
/// coefficient (Neckel & Labs 1994 average about 0.6 over 450-650 nm).
const PENUMBRA_LIMB: f64 = 0.6;
/// The penumbra's lookup averages the transmittance over the visible face (PENUMBRA_NODES rows) while the disc's
/// centre is within PENUMBRA_REACH radii of the limb, handing over to the centre's own from PENUMBRA_BLEND radii up: the
/// face's mean and the centre's differ there by under half a percent, and beyond it the lookup is the centre's as it
/// always was.
const PENUMBRA_NODES: usize = 8;
const PENUMBRA_BLEND: f64 = 2.0;
const PENUMBRA_REACH: f64 = 4.0;
/// Ozone only absorbs, in the Chappuis band (500-700 nm, peaking at 600):
/// over the long grazing paths of twilight that is what keeps the zenith
/// blue. Its absorption cross section in m^2, 380..700 nm every 10 nm
/// (Bruneton's, after Serdyuchenko et al. 2014), and its peak number
/// density in m^-3: 300 Dobson units spread over the tent below.
const OZONE_CROSS_SECTION: [f64; 33] = [
    2.818e-28, 6.636e-28, 1.527e-27, 2.763e-27, 5.52e-27, 8.451e-27, 1.582e-26, 2.316e-26, 3.669e-26,
    4.924e-26, 7.752e-26, 9.016e-26, 1.48e-25, 1.602e-25, 2.139e-25, 2.755e-25, 3.091e-25, 3.5e-25,
    4.266e-25, 4.672e-25, 4.398e-25, 4.701e-25, 5.019e-25, 4.305e-25, 3.74e-25, 3.215e-25, 2.662e-25,
    2.238e-25, 1.852e-25, 1.473e-25, 1.209e-25, 9.423e-26, 7.455e-26,
];
const OZONE_PEAK_DENSITY: f64 = 300.0 * 2.687e20 / 15_000.0;
/// Ozone density is a tent: 1 at 25 km, 0 at 10 km and at 40 km.
const OZONE_PEAK_HEIGHT: f64 = 25_000.0;
const OZONE_HALF_WIDTH: f64 = 15_000.0;

/// Sky-view table. Columns: azimuth away from the sun, 0..pi (the other half
/// of the sky is the mirror image). Rows: view elevation 0..90 deg on a
/// squared scale, so half of the rows sit in the lowest 22 deg, where the sky
/// changes fastest.
const LUT_WIDTH: usize = 128;
const LUT_HEIGHT: usize = 64;
/// Samples along each view ray, crowded toward the viewer.
const VIEW_STEPS: usize = 16;

/// Optical depth toward space, tabulated over height (squared scale) and the
/// angle above the ray that grazes the ground (squared again: the depth grows
/// steeply there). It replaces the light march at every view sample, which
/// keeps `Atmosphere::new` in the low milliseconds in release builds.
const DEPTH_HEIGHTS: usize = 64;
const DEPTH_ANGLES: usize = 64;
const DEPTH_STEPS: usize = 40;
/// Steps for the one-off exact queries (the sun's colour, `transmittance`).
const EXACT_STEPS: usize = 64;

/// Multiple scattering, after Hillaire, "A Scalable and Production Ready Sky
/// and Atmosphere Rendering Technique" (EGSR 2020): at every height and sun
/// angle, the second order of scattering (the sun's light scattered once on
/// the way, plus what the lit ground bounces up, seen from that point in
/// MS_DIRS directions) is summed to all orders as a geometric series, and the
/// view march adds it back as an isotropic source. That is what keeps a real
/// sky's light bright overhead, carries the ground's light into the air and
/// keeps the sky glowing after sunset. Our sun is fixed, so the table only
/// spans the sun angles the view rays can meet: the viewer's own, +- the
/// arc a ray to the top of the dense air crosses (under 8 deg; the far
/// mesosphere's steps take the edge column).
const MS_HEIGHTS: usize = 12;
/// The table works out MS_ANGLES sun angles, evenly over the sun's cosine from cos(z + MS_ANGLE_REACH_DEG) to
/// cos(z - MS_ANGLE_REACH_DEG), z the sun's angle from the viewer's vertical (1.8 deg apart near the horizon). The
/// count is odd so that in twilight a worked-out column lies next to the viewer's own sun angle, which every step of a
/// vertical ray reads. Next to it, not on it: the middle column sits at sin(e) cos(MS_ANGLE_REACH_DEG) for a sun e up,
/// so it is the viewer's own only with the sun on the horizon, within 0.13 of a column of it down to -18 deg
/// (10 columns put the zenith 0.39-0.5 of the way between two), and further off by day, where the source is smooth:
/// 0.23 of a column at 30 deg, 2.2 at 80 (above 81 deg the span stops at the vertical: 3.7 at 85). Below a point's
/// horizon the source falls about 3x per degree, so the table is read on MS_READ_ANGLES columns over the same span,
/// filled between the worked-out ones along each band's logarithm (`read_columns`). Read linearly, 10 columns 2 deg
/// apart read twilight's multiple scattering 1.3-1.75x too bright from -3 to -13 deg (nautical twilight 1.3-1.4x
/// Koomen et al. 1952's measured sky); now it is within 1 % of a table worked out every 0.2 deg by day and
/// down to -6 deg, and within 8 % down to -15 deg (the zenith within 0.1 % down to -6, 1 % at -9 and -12, 3 % at -15).
const MS_ANGLES: usize = 11;
const MS_READ_ANGLES: usize = 91;
/// The source is interpolated over its logarithm with this floor (per unit of the sun's light above the air; the sky
/// 18 degrees into twilight is 1e-11 of it), so a column nothing lights reads as next to nothing.
const PSI_FLOOR: f64 = 1.0e-30;
const MS_DIRS: usize = 32;
const MS_STEPS: usize = 12;
const MS_ANGLE_REACH_DEG: f64 = 9.0;
/// A sun well below a point's horizon lights it only from a band of directions: low toward the sun, where air is still
/// lit far off, and up through the lit mesosphere. MS_DIRS directions cannot integrate that (against 2048 directions,
/// with the sun 15 degrees down they found a third of the light, at 18 none), so the table's columns whose sun is
/// MS_DEEP_FROM_DEG to MS_DEEP_TO_DEG below the point's horizon blend over to MS_DEEP_DIRS directions (on average
/// within 5 % of 2048 down to 15 degrees, 8 % at 18), and the columns above stay as they were. It costs only a sky
/// whose sun is under about 5 degrees (the table then spans a deep column).
const MS_DEEP_DIRS: usize = 256;
const MS_DEEP_FROM_DEG: f64 = 4.0;
const MS_DEEP_TO_DEG: f64 = 8.0;

/// Below the horizon the horizon haze fades into the lit ground over this band.
const GROUND_BLEND_DEG: f32 = 3.0;
/// The sun's limb darkening, Neckel & Labs 1994 (Solar Physics 153, 91), for 422-1100 nm: the radiance at mu (the
/// cosine of the angle between the sun's surface normal and the line of sight) against the centre's is
/// sum_i a_i mu^i, with a_i = A_i + B_i / lambda + C_i / lambda^5, lambda in micrometres; rows [A_i, B_i, C_i] for
/// i = 0..5. Each a_i sums to 1 at mu = 1 at every wavelength. The limb is darker and redder than the centre: at
/// r = 0.999 R it is 0.40, 0.33 and 0.22 of the centre at 650, 550 and 450 nm. The 400 nm band takes the formula
/// 22 nm past its end.
const LIMB_NL94: [[f64; 3]; DISC_LIMB_TERMS] = [
    [0.75267, -0.265577, 0.0],
    [0.93874, 0.265577, -0.004095],
    [-1.89287, 0.0, 0.012582],
    [2.42234, 0.0, -0.017117],
    [-1.71150, 0.0, 0.011977],
    [0.49062, 0.0, -0.003347],
];
/// The terms of the limb darkening's polynomial in mu (`SunDiscLook::limb`).
pub const DISC_LIMB_TERMS: usize = 6;
/// The disc's colour is kept at this many apparent elevations across it (`SunDiscLook::rows`).
pub const DISC_ROWS: usize = 8;
/// The air's refractivity n - 1 at the ground in the visible (15 C, 1013 hPa, 550 nm): with the air's scale height it
/// sets how a ray near the horizon bends (`refracted_transmittance`).
const REFRACTIVITY: f64 = 2.77e-4;

pub struct Atmosphere {
    sun_dir: Vec3f,
    /// Unit horizontal direction of the sun as (x, z); (1, 0) when the sun is
    /// at the zenith, where any reference azimuth will do.
    sun_horizontal: Vec2f,
    medium: Medium,
    /// The sky-view table without the phase (`SkyLut`).
    lut: SkyLut,
    /// The sine and cosine of the sun's elevation as it is seen (`apparent_elevation`), for the view-sun cosine a
    /// lookup applies the phase at: the glow centres on the drawn disc.
    sun_sin: f32,
    sun_cos: f32,
    /// Radiance of the lit ground (Lambertian, ground_color albedo).
    ground: Vec3f,
    /// The light on a level ground, the sky dome's and the sun's (`ground` is ground_color x this / pi).
    ground_light: Vec3f,
    ambient: Vec3f,
    /// The sky's irradiance on a level surface (the disc excluded).
    sky_irradiance: Vec3f,
    sun_irradiance: Vec3f,
    /// The disc as it is drawn: lifted and flattened by the air's refraction, each part coloured by the air its
    /// light crosses, limb-darkened per wavelength (`SunDiscLook`).
    disc: SunDiscLook,
    disc_radius: f32,
    disc_softness: f32,
    /// Chord length round the seen centre (`disc.dir`) beyond which the disc is zero: the outer radius, a little
    /// over (refraction only ever brings the disc's points nearer its seen centre).
    disc_chord: f32,
    disc_visible: bool,
    /// The viewer's height over the ground, metres: VIEWER_HEIGHT unless `AtmosphereParams::viewer_height_m` sets it.
    viewer_m: f64,
    /// What a viewer at a height of its own sees below level (`aerial::ViewerSky`): None (no height set) is the
    /// older ground, blended into the horizon's haze over GROUND_BLEND_DEG.
    below: Option<Box<aerial::ViewerSky>>,
    /// The elevation of the horizon the sky is drawn down to, radians, 0 or below: where the disc is cut and its key
    /// goes. Over the planet's ground seen from a viewer height it is the grazing ray, the dip below level; level
    /// without a height, or under a floor ([`Self::with_level_horizon`]).
    horizon: f32,
    /// What every light this sky gives is lifted by ([`Self::lifted`]): 1 unless it was lifted.
    lift: f32,
}

impl Atmosphere {
    /// The sky for the sun at `sun_dir`. Both blocks are read as clamp() holds
    /// them (`clamped()`, D17), so their ranges, defaults and NaN rule are
    /// their `clamp_fields`' alone.
    pub fn new(sun_dir: Vec3f, atmo: &AtmosphereParams, disc: &SunDiscParams) -> Atmosphere {
        let (atmo, disc) = (&atmo.clamped(), &disc.clamped());
        let sun = sun_dir.normalize();
        let sun = if sun.is_finite() && sun.length() > 0.5 { sun } else { vec3f(0.0, 1.0, 0.0) };
        let horizontal = (sun.x * sun.x + sun.z * sun.z).sqrt();
        let sun_horizontal = if horizontal > 1.0e-6 {
            vec2f(sun.x / horizontal, sun.z / horizontal)
        } else {
            vec2f(1.0, 0.0)
        };
        let medium = Medium::new(atmo);
        let table = DepthTable::new(&medium);
        let sun_sin = sun.y.clamp(-1.0, 1.0);
        let multi = MultiScatter::new(&medium, &table, sun_sin as f64);
        let viewer = atmo.viewer_height_m.map(|h| h as f64);
        let viewer_m = viewer.unwrap_or(VIEWER_HEIGHT);
        let lut = match viewer {
            Some(h) => aerial::sky_lut(&medium, &table, sun_sin as f64, &multi, PLANET_RADIUS + h),
            None => build_sky_lut(&medium, &table, sun_sin as f64, &multi),
        };
        // The sun's light arrives along the bent path from where it is seen (`refracted_transmittance`): near the
        // horizon that path crosses less air than the straight one toward the true sun, and it still carries light
        // while the true sun is a little below the horizon.
        let seen = apparent_elevation(sun.y.clamp(-1.0, 1.0).asin());
        // The table's planes are the march toward the true sun (the Earth's shadow, the light's path through the air),
        // but the glow round the sun is mostly the low haze's scattering of light that reached it along that bent
        // path, so a lookup applies the phase about the sun as it is seen: the glow centres on the drawn disc (0.57
        // deg above the true sun on the horizon, under a hundredth of a degree at 57 deg).
        let (seen_sin, seen_cos) = seen.sin_cos();
        let (ambient, sky_irradiance) = hemisphere_integrals(&|phi, el| lut.at(phi, el, seen_sin, seen_cos));
        let sun_irradiance = medium.rgb(&refracted_transmittance_from(&medium, seen as f64, viewer_m));
        let ground_color = vec3f(atmo.ground_color[0], atmo.ground_color[1], atmo.ground_color[2]);
        let below = viewer.map(|h| Box::new(aerial::ViewerSky::new(&medium, &table, &multi, sun_sin as f64, PLANET_RADIUS + h, ground_color)));
        // (from a viewer height the planet's ground meets the sky at the grazing ray)
        let horizon = below.as_ref().map_or(0.0, |below| -below.dip());
        // A Lambertian ground under the sky dome and the sun (from where its light arrives): albedo x E / pi.
        let ground_light = sky_irradiance + sun_irradiance * seen.sin().max(0.0);
        let ground = ground_color * ground_light * (1.0 / std::f32::consts::PI);
        let disc_radius = (0.5 * disc.size_deg).to_radians();
        let disc_softness = disc.softness;
        let outer = disc_radius * (1.0 + 0.5 * disc_softness);
        let disc_look = sun_disc_look(&medium, sun, disc_radius, disc_softness, sun_irradiance, horizon, viewer);
        Atmosphere {
            sun_dir: sun,
            sun_horizontal,
            medium,
            lut,
            sun_sin: seen_sin,
            sun_cos: seen_cos,
            ground,
            ground_light,
            ambient,
            sky_irradiance,
            sun_irradiance,
            disc: disc_look,
            disc_radius,
            disc_softness,
            disc_chord: 2.0 * (0.5 * (outer * 1.01 + 1.0e-6)).min(1.5).sin(),
            disc_visible: disc.visible,
            viewer_m,
            below,
            horizon,
            lift: 1.0,
        }
    }

    /// This sky under a floor, whose ground runs to level and stands in front of the sky below it: the disc is cut at
    /// level and its key goes there, as without a viewer height, not at the planet's dipped horizon. Without a height
    /// the sky is returned as it is.
    pub fn with_level_horizon(mut self) -> Atmosphere {
        if self.horizon < 0.0 {
            self.horizon = 0.0;
            let viewer = self.below.as_ref().map(|_| self.viewer_m);
            self.disc = sun_disc_look(&self.medium, self.sun_dir, self.disc_radius, self.disc_softness, self.sun_irradiance, 0.0, viewer);
        }
        self
    }

    /// This sky with every light it gives lifted by `k` (the night's adaptation, `night::adaptation`: 1 with the sun up,
    /// up to 2^10 once it is 12 degrees down): the sky, the ground below the horizon and the air in front of any
    /// ground, the sun's disc and its light at every height, the ambient and the irradiances. Its transmittance stays,
    /// so a ground a caller lit by this sky is seen through the air as it was. The night layers are lifted by the same
    /// factor, so twilight keeps its real ratio to them, and whatever this sky lights (the clouds, the floors, the key,
    /// the dome's light) follows it. At 1 (by day) the sky is returned as it is, bit for bit.
    pub fn lifted(mut self, k: f32) -> Atmosphere {
        if k == 1.0 || !(k.is_finite() && k > 0.0) {
            return self;
        }
        self.lift *= k;
        self.lut.scale(k);
        if let Some(below) = self.below.as_mut() {
            below.scale(k);
        }
        self.ground *= k;
        self.ground_light *= k;
        self.ambient *= k;
        self.sky_irradiance *= k;
        self.sun_irradiance *= k;
        // (the disc is linear in its light: `sun_disc_look` scales its rows to it)
        self.disc.rows = self.disc.rows.map(|row| row * k);
        self
    }

    /// Sky radiance from `dir` without the disc: in-scattering plus the
    /// multiple-scattering term above the horizon, the lit ground below it.
    pub fn sky(&self, dir: Vec3f) -> Vec3f {
        let horizontal = (dir.x * dir.x + dir.z * dir.z).sqrt();
        // atan2 rather than asin(y): exact for non-unit input and at the zenith.
        let elevation = dir.y.atan2(horizontal);
        let phi = self.relative_azimuth(dir);
        if elevation >= 0.0 {
            return self.lut.at(phi, elevation, self.sun_sin, self.sun_cos);
        }
        if let Some(below) = &self.below {
            return below.below(phi, elevation, self.sun_sin, self.sun_cos, None);
        }
        let haze = self.lut.at(phi, 0.0, self.sun_sin, self.sun_cos);
        let t = smoothstep(0.0, GROUND_BLEND_DEG.to_radians(), -elevation);
        haze.mix(self.ground, t)
    }

    /// [`Self::sky`] over a ground of albedo `albedo` instead of ground_color (the floor's far model, plan hdri-8),
    /// lit as `sky` lights its own, its blend into the horizon haze scaled by `keep` (0..1: 1 is `sky`'s blend, less
    /// leaves more of the haze, a far valley's aerial perspective). At albedo ground_color and keep 1 it is `sky`, bit
    /// for bit. Above the horizon it is `sky`. With a viewer height set the ground is the planet's, of albedo
    /// `albedo`, seen through the air by its own distance (`keep` is the older blend's and is left out).
    pub fn sky_on_ground(&self, dir: Vec3f, albedo: Vec3f, keep: f32) -> Vec3f {
        let horizontal = (dir.x * dir.x + dir.z * dir.z).sqrt();
        let elevation = dir.y.atan2(horizontal);
        let phi = self.relative_azimuth(dir);
        if elevation >= 0.0 {
            return self.lut.at(phi, elevation, self.sun_sin, self.sun_cos);
        }
        if let Some(below) = &self.below {
            return below.below(phi, elevation, self.sun_sin, self.sun_cos, Some(albedo));
        }
        let haze = self.lut.at(phi, 0.0, self.sun_sin, self.sun_cos);
        let t = smoothstep(0.0, GROUND_BLEND_DEG.to_radians(), -elevation) * keep;
        haze.mix(self.ground_radiance(albedo), t)
    }

    /// The radiance of a level Lambertian ground of albedo `albedo` under this sky and sun (`ground` at ground_color).
    pub fn ground_radiance(&self, albedo: Vec3f) -> Vec3f {
        albedo * self.ground_light * (1.0 / std::f32::consts::PI)
    }

    /// [`Self::sky_on_ground`] over a ground of radiance `ground` (before the haze blend): the floor's planes, lit on
    /// their own (plan hdri-8 T2). `sky_on_ground(dir, a, keep)` is `sky_over(dir, ground_radiance(a), keep)`, bit for
    /// bit. With a viewer height set `ground` is where the ray meets the planet's ground, seen through the air to
    /// there (`keep` is left out); a floor, nearer, takes [`Self::sky_over_at`].
    pub fn sky_over(&self, dir: Vec3f, ground: Vec3f, keep: f32) -> Vec3f {
        let horizontal = (dir.x * dir.x + dir.z * dir.z).sqrt();
        let elevation = dir.y.atan2(horizontal);
        let phi = self.relative_azimuth(dir);
        if elevation >= 0.0 {
            return self.lut.at(phi, elevation, self.sun_sin, self.sun_cos);
        }
        if let Some(below) = &self.below {
            return below.over(phi, elevation, self.sun_sin, self.sun_cos, ground);
        }
        let haze = self.lut.at(phi, 0.0, self.sun_sin, self.sun_cos);
        let t = smoothstep(0.0, GROUND_BLEND_DEG.to_radians(), -elevation) * keep;
        haze.mix(ground, t)
    }

    /// [`Self::sky_over`] over a ground `dist_m` metres away along `dir` (a floor's texel, whose distance its floor
    /// knows). With a viewer height set (`AtmosphereParams::viewer_height_m`) the ground is seen through the air over
    /// that distance, once: what the air scatters toward the viewer on the way plus the ground times the air's
    /// transmittance, and `keep` (the older blend's stand-in for that air) is left out. Without one it is `sky_over`,
    /// bit for bit.
    pub fn sky_over_at(&self, dir: Vec3f, ground: Vec3f, keep: f32, dist_m: f32) -> Vec3f {
        let Some(below) = &self.below else {
            return self.sky_over(dir, ground, keep);
        };
        let horizontal = (dir.x * dir.x + dir.z * dir.z).sqrt();
        let elevation = dir.y.atan2(horizontal);
        let phi = self.relative_azimuth(dir);
        if elevation >= 0.0 {
            return self.lut.at(phi, elevation, self.sun_sin, self.sun_cos);
        }
        below.through(phi, elevation, dist_m, self.sun_sin, self.sun_cos, ground)
    }

    /// How far below level the horizon the sky is drawn down to lies, radians: the ray that grazes the planet's ground
    /// from the viewer's height (0.45 deg from 200 m, 1.4 deg from 2000 m). The disc is cut there and the key goes
    /// there; the night's stars, moon and glow end there. 0 without a viewer height set, whose ground meets the sky at
    /// level, and under a floor ([`Self::with_level_horizon`]).
    pub fn horizon_dip(&self) -> f32 {
        0.0 - self.horizon
    }

    /// The viewer's height over the ground in metres when `AtmosphereParams::viewer_height_m` sets one; None for the
    /// calibrated sky, whose ground below the horizon blends into its haze.
    pub fn viewer_height(&self) -> Option<f32> {
        self.below.as_ref().map(|_| self.viewer_m as f32)
    }

    /// The air's own light along `dir` below level seen from a viewer height (`AtmosphereParams::viewer_height_m`),
    /// without the ground behind it: in front of the planet's ground, what [`Self::sky`] adds to that ground's light
    /// through the air (between level and the grazing ray, where the ray meets no ground, all of `sky`), or with
    /// `dist_m`, in front of a ground that far away ([`Self::sky_over_at`]'s). Its two parts apart ([`AirLight`]: the
    /// sun's light scattered once, the multiple scattering); `AirLight::total` is the light `sky` adds. None without a
    /// viewer height (the older ground blends the horizon's haze into the ground over GROUND_BLEND_DEG instead) and at or
    /// above level.
    pub fn air_light(&self, dir: Vec3f, dist_m: Option<f32>) -> Option<AirLight> {
        let below = self.below.as_ref()?;
        let horizontal = (dir.x * dir.x + dir.z * dir.z).sqrt();
        let elevation = dir.y.atan2(horizontal);
        if !(elevation < 0.0) {
            return None;
        }
        let phi = self.relative_azimuth(dir);
        Some(match dist_m {
            Some(dist) => below.air_through(phi, elevation, dist, self.sun_sin, self.sun_cos),
            None => below.air_below(phi, elevation, self.sun_sin, self.sun_cos),
        })
    }

    /// The light on a level ground, the sky dome's and the sun's (irradiance; the ground's radiance is albedo x this / pi).
    pub fn ground_light(&self) -> Vec3f {
        self.ground_light
    }

    /// The sun disc: zero outside it, below the horizon ([`Self::horizon_dip`]), or when hidden.
    pub fn sun_disc(&self, dir: Vec3f) -> Vec3f {
        // (from a viewer height over the planet's ground the horizon lies below level)
        let cut = if self.horizon < 0.0 { self.horizon.sin() * dir.length() } else { 0.0 };
        if !self.disc_visible || dir.y < cut {
            return Vec3f::default();
        }
        let d = dir.normalize();
        // Most of the map is far from the disc: a chord round its seen centre turns it away before any refraction.
        if !((d - self.disc.dir).length() < self.disc_chord) {
            return Vec3f::default();
        }
        self.disc.at(d)
    }

    /// The disc's centre as seen: the sun's direction lifted by the air's refraction ([`refraction`]), 34' on the
    /// horizon. The sun's light arrives from here: the key, its glints and the clouds' lighting take it.
    /// [`Self::sun_dir`] stays the sun's own direction, which the sky's scattering and the night's twilight take.
    pub fn disc_dir(&self) -> Vec3f {
        self.disc.dir
    }

    /// The mean of `pass` over the disc as it is drawn, per channel, each point weighted by the disc's light there in
    /// that channel (its edge, limb darkening and colour row; the horizon's cut): when `pass` is what something passes
    /// along a direction, the share of the disc's light it lets through in each channel, even where an edge crosses
    /// the disc (the lower limb is redder, so a cloud over it takes more green and blue than red). A sunflower spiral
    /// of POINTS points over the disc's outer edge round its seen centre (the drawn disc lies inside it: the
    /// refraction only flattens it), as the cloud stack's cone mean (`CloudStack::disc_transmittance`) takes; 512 keep
    /// a cloud's edge across a 0.53 degree disc within half a percent of a fine grid. Hidden or not, the disc's shape is
    /// used; the centre sample when no point of it is above the horizon.
    pub fn disc_mean(&self, pass: &dyn Fn(Vec3f) -> f32) -> Vec3f {
        const POINTS: usize = 512;
        const GOLDEN_ANGLE: f32 = 2.399_963;
        let centre = self.disc.dir.normalize();
        let helper = if centre.y.abs() < 0.99 { vec3f(0.0, 1.0, 0.0) } else { vec3f(1.0, 0.0, 0.0) };
        let right = Vec3f::cross(helper, centre).normalize();
        let up = Vec3f::cross(centre, right);
        let reach = self.disc.outer().min(0.5).tan();
        let cut = self.horizon.sin();
        let (mut sum, mut weights) = (Vec3f::default(), Vec3f::default());
        for k in 0..POINTS {
            let r = ((k as f32 + 0.5) / POINTS as f32).sqrt();
            let (s, c) = (k as f32 * GOLDEN_ANGLE).sin_cos();
            let d = (centre + right * (r * c * reach) + up * (r * s * reach)).normalize();
            if d.y < cut {
                continue;
            }
            let w = self.disc.at(d);
            if w.is_finite() && w.max_elem() > 0.0 {
                sum += w * pass(d);
                weights += w;
            }
        }
        let mean = |s: f32, w: f32| if w > 0.0 { s / w } else { f32::NAN };
        let m = vec3f(mean(sum.x, weights.x), mean(sum.y, weights.y), mean(sum.z, weights.z));
        if m.is_finite() { m } else { Vec3f::all(pass(centre)) }
    }

    /// Transmittance from the viewer to space along `dir`; zero when the
    /// ground is in the way.
    pub fn transmittance(&self, dir: Vec3f) -> Vec3f {
        self.medium.rgb(&exact_transmittance(&self.medium, dir, self.viewer_m)) * (1.0 / SUN_IRRADIANCE)
    }

    /// SUN_IRRADIANCE x transmittance along the sun: what a surface facing
    /// the sun receives at the ground.
    pub fn sun_irradiance(&self) -> Vec3f {
        self.sun_irradiance
    }

    /// What a surface facing the sun receives `height_m` above the ground
    /// (the viewer's own height or more): the light a cloud deck is lit by.
    /// Up there the sun crosses less of the haze, which sits low, so a low
    /// sun's light on the clouds is far brighter than at the ground; and a
    /// sun just under the viewer's horizon still lights a deck high enough.
    /// The light arrives along the bent path from where the sun is seen, as `sun_irradiance`'s (which this is at the
    /// viewer's own height), lifted as the sky is ([`Self::lifted`]).
    pub fn sun_irradiance_at(&self, height_m: f32) -> Vec3f {
        let seen = apparent_elevation(self.sun_dir.y.clamp(-1.0, 1.0).asin()) as f64;
        self.medium.rgb(&refracted_transmittance_from(&self.medium, seen, (height_m as f64).max(self.viewer_m))) * self.lift
    }

    /// Radiance at the centre of the disc (limb darkening makes it the peak).
    pub fn sun_radiance(&self) -> Vec3f {
        self.disc.at(self.disc.dir)
    }

    /// Mean radiance over the nominal disc cone (radius size_deg / 2):
    /// radiance x 2 pi (1 - sun_cos_radius()) is exactly `sun_irradiance()`.
    /// This is what the engine's key light wants. The cone is the one the
    /// f32 `sun_cos_radius()` names, cancellation and all, because that is
    /// the cone every consumer of the key computes (`EnvSun::cone_radiance`):
    /// a cancellation-free cone here lit a 0.1 degree sun 6 % too dark.
    pub fn sun_cone_radiance(&self) -> Vec3f {
        EnvSun::cone_radiance(self.sun_irradiance, self.sun_cos_radius())
    }

    pub fn sun_cos_radius(&self) -> f32 {
        self.disc_radius.cos()
    }

    /// Angular radius of the disc's outer edge (the radius plus half its soft edge), in radians.
    pub fn sun_outer_radius(&self) -> f32 {
        self.disc_radius * (1.0 + 0.5 * self.disc_softness)
    }

    /// The disc as [`Self::sun_disc`] draws it, for a viewer that draws it
    /// itself (`Env::sky_discs`): None when it is hidden. Its radiance along a
    /// direction is [`SunDiscLook::at`], cut at the horizon ([`Self::horizon_dip`]).
    pub fn disc_look(&self) -> Option<SunDiscLook> {
        self.disc_visible.then_some(self.disc)
    }

    /// Mean radiance of the upper hemisphere (the disc excluded).
    pub fn ambient(&self) -> Vec3f {
        self.ambient
    }

    /// The irradiance the sky (the disc excluded) puts on a level surface:
    /// the ground's light from above, the sun's apart.
    pub fn sky_irradiance(&self) -> Vec3f {
        self.sky_irradiance
    }

    /// Unit direction toward the sun, in the map's own frame (before the
    /// map rotation).
    pub fn sun_dir(&self) -> Vec3f {
        self.sun_dir
    }

    /// The sun's light at the ground band by band (`SunBands`): what a light that takes a share of each band of it
    /// shows, a rainbow's (rainbow.rs).
    pub(crate) fn sun_bands(&self) -> SunBands {
        let seen = apparent_elevation(self.sun_dir.y.clamp(-1.0, 1.0).asin()) as f64;
        let through = refracted_transmittance_from(&self.medium, seen, self.viewer_m);
        let bands = std::array::from_fn(|k| {
            let mut one = [0.0; BANDS];
            one[k] = through[k];
            self.medium.rgb_raw(&one) * self.lift
        });
        SunBands { bands, blue_limit: self.medium.blue_limit }
    }

    /// Angle between the view's and the sun's horizontal directions, 0..pi.
    fn relative_azimuth(&self, dir: Vec3f) -> f32 {
        let along = dir.x * self.sun_horizontal.x + dir.z * self.sun_horizontal.y;
        let across = dir.x * self.sun_horizontal.y - dir.z * self.sun_horizontal.x;
        // |across| folds the mirror half onto the stored one; atan2(0, 0) = 0
        // at the zenith, where every column holds the same value anyway.
        across.abs().atan2(along)
    }
}

/// The sun's light at the ground split into the bands (`Atmosphere::sun_bands`): each band's light alone as linear
/// Rec.709 (`rgb_raw`, its blue below zero where the matching functions' lobes put it), so they add up to the sun's
/// light before its gamut handling.
pub(crate) struct SunBands {
    pub(crate) bands: [Vec3f; BANDS],
    blue_limit: f32,
}

impl SunBands {
    /// The light that takes the share `s[k]` of each band of the sun's: drawn inside Rec.709 as `Medium::light_rgb`
    /// draws a light, and what still lies outside it (a band's alone, the deep red's green and the violet's, below
    /// zero) held at zero.
    pub(crate) fn light(&self, s: &[f32; BANDS]) -> Vec3f {
        let raw = (0..BANDS).fold(Vec3f::default(), |sum, k| sum + self.bands[k] * s[k]);
        let c = compress_blue(raw, self.blue_limit);
        vec3f(c.x.max(0.0), c.y.max(0.0), c.z.max(0.0))
    }
}

/// Sun direction (map frame) for the sun parameters: the NOAA position in
/// time mode through `crate::sky::noaa_solar_position`, the manual angles
/// otherwise. NOAA's azimuth is clockwise from north, `dir_from_az_el`'s
/// sense, so no conversion sits between them. The sun is read as clamp()
/// holds it (`SunParams::clamped`, D17): its month is 1..=12 and its day
/// within that month, so both fit the date's bytes.
pub fn sun_direction(sun: &SunParams) -> Vec3f {
    let sun = &sun.clamped();
    match sun.mode() {
        SunMode::Time => {
            let date = SkyDate { year: sun.year, month: sun.month as u8, day: sun.day as u8 };
            let (elevation, azimuth) =
                noaa_solar_position(date, sun.hour, sun.tz_offset, sun.latitude, sun.longitude);
            dir_from_az_el(azimuth, elevation)
        }
        SunMode::Manual => dir_from_az_el(sun.azimuth_deg, sun.elevation_deg),
    }
}

/// Scattering and absorption coefficients for one parameter set, per metre
/// at sea level, per band; and the bands' way back to the scene's RGB.
#[derive(Clone, Copy)]
struct Medium {
    /// The low haze's kind: its colour, absorption and phase (the stratosphere's haze keeps the continental's).
    aerosol: Aerosol,
    rayleigh: Spectrum,
    mie_scattering: Spectrum,
    mie_extinction: Spectrum,
    strat_scattering: Spectrum,
    strat_extinction: Spectrum,
    ozone: Spectrum,
    /// The ground's albedo, for the light it bounces back into the air.
    ground: Spectrum,
    to_rgb: [Spectrum; 3],
    /// The boundary layer's top in metres (`AtmosphereParams::haze_top_m`); None: the haze falls exponentially
    /// with MIE_SCALE_HEIGHT.
    haze_top: Option<f64>,
    /// The farthest any light the bands carry lies outside Rec.709 in blue, a mix of two bands (`blue_limit`):
    /// `compress_blue` draws it onto the gamut's edge and every light nearer than it inside.
    blue_limit: f32,
}

impl Medium {
    /// `p` is held by clamp() (`Atmosphere::new`).
    fn new(p: &AtmosphereParams) -> Medium {
        let (air, haze, ozone) = (p.air as f64, p.haze as f64, p.ozone as f64);
        let strat = p.stratosphere.unwrap_or(1.0) as f64;
        let aerosol = Aerosol::named(p.aerosol.as_deref().unwrap_or("continental"));
        let haze_top = p.haze_top_m.map(|top| top as f64);
        // The haze's column per unit of its density at the ground, so its optical depth is the Haze control's for any
        // profile: the exponential's scale height, or the layer plus the free troposphere over it.
        let column = haze_top.map_or(MIE_SCALE_HEIGHT, |top| top + FREE_HAZE_SHARE * FREE_HAZE_SCALE_HEIGHT);
        let mie_extinction: Spectrum = std::array::from_fn(|k| {
            haze * AEROSOL_DEPTH / column * (BAND_NM[k] / 550.0).powf(-aerosol.angstrom)
        });
        let strat_extinction: Spectrum = std::array::from_fn(|k| {
            strat * STRAT_DEPTH / STRAT_HALF_WIDTH * (BAND_NM[k] / 550.0).powf(-AEROSOL_ANGSTROM)
        });
        let cross = band_ozone_cross_sections();
        // The ground's colour spread over the bands: blue at 440 nm, green at
        // 550, red at 640, linear between and held beyond.
        let [gr, gg, gb] = p.ground_color.map(|g| g as f64);
        let ground = BAND_NM.map(|nm| {
            if nm <= 440.0 {
                gb
            } else if nm <= 550.0 {
                gb + (gg - gb) * (nm - 440.0) / 110.0
            } else if nm <= 640.0 {
                gg + (gr - gg) * (nm - 550.0) / 90.0
            } else {
                gr
            }
        });
        let to_rgb = band_to_rgb();
        Medium {
            aerosol,
            // Rayleigh: 1.24062e-6 / lambda^4 per metre, lambda in um (Bruneton's air).
            rayleigh: BAND_NM.map(|nm| air * 1.24062e-6 / (nm / 1000.0).powi(4)),
            mie_scattering: std::array::from_fn(|k| mie_extinction[k] * aerosol.albedo[k]),
            mie_extinction,
            strat_scattering: strat_extinction.map(|e| e * AEROSOL_ALBEDO),
            strat_extinction,
            ozone: cross.map(|c| c * OZONE_PEAK_DENSITY * ozone),
            ground,
            to_rgb,
            haze_top,
            blue_limit: blue_limit(&to_rgb),
        }
    }

    /// The relative densities over a straight step `ds` long centred at radius `r`, its direction at cosine `mu` to
    /// the vertical there: the air's, the ozone's and the stratosphere's haze at the centre (`densities`), and the
    /// low haze's from its profile. A boundary layer's even haze counts the share of the step under its top exactly
    /// (`share_inside`), so a march of a few long steps keeps the layer's sharp top and its column; the free
    /// troposphere's thin haze over it is read at the centre; `with_layer` false leaves the layer's even haze out (the
    /// depth table takes it along each lookup's ray instead). Without a layer it is `densities` at the centre, bit for
    /// bit.
    fn densities_over(&self, r: f64, mu: f64, ds: f64, with_layer: bool) -> Densities {
        let mut d = densities(r - PLANET_RADIUS);
        if let Some(top) = self.haze_top {
            let over = (r - PLANET_RADIUS - top).max(0.0);
            let even = if with_layer { (1.0 - FREE_HAZE_SHARE) * share_inside(r, mu, ds, PLANET_RADIUS + top) } else { 0.0 };
            d[1] = even + FREE_HAZE_SHARE * (-over / FREE_HAZE_SCALE_HEIGHT).exp();
        }
        d
    }

    /// Per-band extinction for (air, haze, ozone, stratospheric haze)
    /// densities: a coefficient per metre for point densities, an optical
    /// depth for integrated ones.
    fn extinction(&self, d: Densities) -> Spectrum {
        std::array::from_fn(|k| {
            self.rayleigh[k] * d[0] + self.mie_extinction[k] * d[1] + self.ozone[k] * d[2] + self.strat_extinction[k] * d[3]
        })
    }

    /// Per-band scattering coefficient for point densities.
    fn scattering(&self, d: Densities) -> Spectrum {
        std::array::from_fn(|k| self.rayleigh[k] * d[0] + self.mie_scattering[k] * d[1] + self.strat_scattering[k] * d[3])
    }

    /// Per-band scattering by the low haze, which takes its kind's phase (the
    /// stratosphere's, `strat_scattering`, is kept apart: it takes the continental's).
    fn haze_scattering(&self, d: Densities) -> Spectrum {
        std::array::from_fn(|k| self.mie_scattering[k] * d[1])
    }

    /// A spectrum, per unit of the sun's light at the top of the air, as the
    /// scene's linear Rec.709 RGB: SUN_IRRADIANCE per channel for the sun
    /// itself. A deeply reddened spectrum (a sun on the horizon) lies outside
    /// Rec.709 and its blue would fall below zero (the matching functions'
    /// negative lobes); it is drawn inside as a camera records it
    /// (`light_rgb`), and a channel still below a millionth of the colour's
    /// sum is held there, never exactly 0, which a light's colour is divided by.
    fn rgb(&self, s: &Spectrum) -> Vec3f {
        gamut_floor(self.light_rgb(s))
    }

    /// The colour of one light, `rgb_raw` drawn inside Rec.709 where its blue falls short (`compress_blue`): what a
    /// camera records of it. A light reddened by more than about 20 air masses (a sun under 2 degrees, the haze's light
    /// round it) lies outside Rec.709 and its blue comes out negative, -0.18 of the green for a 1 degree sun; a camera
    /// records such a light inside, with a little blue (zwartkops sunset's unclipped 1 degree disc: b/g 0.13, b/r
    /// 0.032; its aureole 0.5-2 degrees out b/r 0.026-0.042). Lights are drawn in one by one before they are mixed
    /// (the sky-view table's planes, the sun): summed first, the reddened light's negative blue took the blue of the
    /// sky's other light with it, and within 4 degrees of a low sun the glow had none.
    fn light_rgb(&self, s: &Spectrum) -> Vec3f {
        compress_blue(self.rgb_raw(s), self.blue_limit)
    }

    /// `rgb` as the matching functions give it, without the gamut's handling: linear in the spectrum, so a sum of
    /// these is the sum's, and a reddened light's blue below zero.
    fn rgb_raw(&self, s: &Spectrum) -> Vec3f {
        let c: [f64; 3] = std::array::from_fn(|c| {
            let mut sum = 0.0;
            for k in 0..BANDS {
                sum += self.to_rgb[c][k] * s[k];
            }
            sum * SUN_IRRADIANCE as f64
        });
        vec3f(c[0] as f32, c[1] as f32, c[2] as f32)
    }
}

/// The weight of each 10 nm sample (from 380 nm) in each band: 1 inside it,
/// a half on a border it shares with the next band.
fn band_share(i: usize, k: usize) -> f64 {
    let nm = 380.0 + 10.0 * i as f64;
    let d = (nm - BAND_NM[k]).abs();
    if d < 19.9 {
        1.0
    } else if d < 20.1 {
        0.5
    } else {
        0.0
    }
}

/// Each band's ozone cross section: the mean over its 10 nm samples.
fn band_ozone_cross_sections() -> Spectrum {
    std::array::from_fn(|k| {
        let (mut sum, mut weight) = (0.0, 0.0);
        for (i, c) in OZONE_CROSS_SECTION.iter().enumerate() {
            let w = band_share(i, k);
            sum += w * c;
            weight += w;
        }
        sum / weight
    })
}

/// The bands' weights in linear Rec.709: the CIE 1931 colour-matching
/// functions times the sun's spectrum at the top of the air, summed over
/// each band and taken to RGB, each channel scaled so that the sun's own
/// spectrum (1 in every band) is white. White is therefore the sun above
/// the air, the scene's white as it always was.
fn band_to_rgb() -> [Spectrum; 3] {
    const XYZ_TO_RGB: [[f64; 3]; 3] = [
        [3.2406, -1.5372, -0.4986],
        [-0.9689, 1.8758, 0.0415],
        [0.0557, -0.2040, 1.0570],
    ];
    let mut w = [[0.0f64; BANDS]; 3];
    for (i, (xyz, sun)) in CIE_XYZ.iter().zip(SOLAR_SPECTRUM).enumerate() {
        for k in 0..BANDS {
            let share = band_share(i, k) * sun;
            if share == 0.0 {
                continue;
            }
            for c in 0..3 {
                let m = XYZ_TO_RGB[c];
                w[c][k] += share * (m[0] * xyz[0] + m[1] * xyz[1] + m[2] * xyz[2]);
            }
        }
    }
    for row in &mut w {
        let total: f64 = row.iter().sum();
        row.iter_mut().for_each(|v| *v /= total);
    }
    w
}

/// Relative densities: air, haze, ozone and the stratosphere's haze.
type Densities = [f64; 4];

/// Relative densities at `height` metres above the ground.
fn densities(height: f64) -> Densities {
    let h = height.max(0.0);
    const MESOSPHERE_FLOOR: f64 = TOP_RADIUS - PLANET_RADIUS;
    let air = if h > MESOSPHERE_FLOOR {
        MESOSPHERE_DENSITY * (-(h - MESOSPHERE_FLOOR) / MESOSPHERE_SCALE_HEIGHT).exp()
    } else {
        (-h / RAYLEIGH_SCALE_HEIGHT).exp()
    };
    [
        air,
        (-h / MIE_SCALE_HEIGHT).exp(),
        (1.0 - (h - OZONE_PEAK_HEIGHT).abs() / OZONE_HALF_WIDTH).max(0.0),
        (1.0 - (h - STRAT_PEAK_HEIGHT).abs() / STRAT_HALF_WIDTH).max(0.0),
    ]
}

/// The share of a straight step `ds` long, centred at radius `r` with its direction at cosine `mu` to the vertical
/// there, that lies inside the sphere of radius `rb`: the chord the sphere cuts from the step's line, exactly. Along
/// the line, |p(t)|^2 = r^2 + 2 r mu t + t^2 is under rb^2 for t within -r mu +- q, q^2 = (rb - r)(rb + r) + (r mu)^2.
fn share_inside(r: f64, mu: f64, ds: f64, rb: f64) -> f64 {
    if !(ds > 0.0) {
        return if r < rb { 1.0 } else { 0.0 };
    }
    let q2 = (rb - r) * (rb + r) + (r * mu) * (r * mu);
    if !(q2 > 0.0) {
        return 0.0;
    }
    let q = q2.sqrt();
    let half = 0.5 * ds;
    ((q.min(r * mu + half) - (-q).max(r * mu - half)) / ds).clamp(0.0, 1.0)
}

/// True when a ray from radius `r` with cosine `mu` against the local
/// vertical runs into the planet.
fn hits_ground(r: f64, mu: f64) -> bool {
    mu < 0.0 && r * r * (1.0 - mu * mu) < PLANET_RADIUS * PLANET_RADIUS
}

/// Distance from radius `r` (inside the shell) along `mu` to the top.
fn distance_to_top(r: f64, mu: f64) -> f64 {
    let disc = r * r * (mu * mu - 1.0) + TOP_RADIUS * TOP_RADIUS;
    (-r * mu + disc.max(0.0).sqrt()).max(0.0)
}

/// Distance from radius `r` (inside the shell) along `mu` to the mesosphere's top.
fn distance_to_mesosphere_top(r: f64, mu: f64) -> f64 {
    let disc = r * r * (mu * mu - 1.0) + MESOSPHERE_RADIUS * MESOSPHERE_RADIUS;
    (-r * mu + disc.max(0.0).sqrt()).max(0.0)
}

/// Distance from radius `r` along `mu` to the ground (the ray must hit it).
fn distance_to_ground(r: f64, mu: f64) -> f64 {
    let disc = r * r * (mu * mu - 1.0) + PLANET_RADIUS * PLANET_RADIUS;
    (-r * mu - disc.max(0.0).sqrt()).max(0.0)
}

/// Radius after travelling `t` from radius `r` along `mu`.
fn radius_at(r: f64, mu: f64, t: f64) -> f64 {
    (r * r + 2.0 * r * mu * t + t * t).sqrt()
}

/// Cosine of the ray from radius `r` that just grazes the ground.
fn grazing_mu(r: f64) -> f64 {
    let s = (PLANET_RADIUS / r).min(1.0);
    -(1.0 - s * s).max(0.0).sqrt()
}

/// Midpoint and length of segment `i` of `n` over `length`, spaced
/// quadratically: samples crowd the start of the ray, where the air is
/// densest for every ray that starts low.
fn segment(i: usize, n: usize, length: f64) -> (f64, f64) {
    let u0 = i as f64 / n as f64;
    let u1 = (i + 1) as f64 / n as f64;
    let t0 = length * u0 * u0;
    let t1 = length * u1 * u1;
    (0.5 * (t0 + t1), t1 - t0)
}

/// Segment `i` of a march that takes `n` steps over the dense air's `length` (`segment`) and MESOSPHERE_STEPS more
/// over the mesosphere's `beyond`, which follows it on the same ray.
fn march_segment(i: usize, n: usize, length: f64, beyond: f64) -> (f64, f64) {
    if i < n {
        return segment(i, n, length);
    }
    let (t, ds) = segment(i - n, MESOSPHERE_STEPS, beyond);
    (length + t, ds)
}

/// Integrated densities in metres from radius `r` along `mu` to space, or
/// None when the ground is in the way.
fn column_depth(medium: &Medium, r: f64, mu: f64, steps: usize) -> Option<Densities> {
    if hits_ground(r, mu) {
        return None;
    }
    Some(integrate_column(medium, r, mu, steps, true))
}

/// The same integral without the ground test. The depth table needs it for
/// its grazing column: there `hits_ground` sits exactly on its boundary and
/// rounding could turn a ray that skims the ground into "blocked", which
/// would store a depth of 0 (full sunlight) right at the terminator.
fn integrate_column(medium: &Medium, r: f64, mu: f64, steps: usize, with_layer: bool) -> Densities {
    let length = distance_to_top(r, mu);
    let mut depth = [0.0f64; 4];
    for i in 0..steps {
        let (t, ds) = segment(i, steps, length);
        // `densities` clamps the height at 0, so a skimming ray that rounds a
        // hair below the surface reads sea-level air.
        let rp = radius_at(r, mu, t);
        let d = medium.densities_over(rp, (r * mu + t) / rp, ds, with_layer);
        for (acc, v) in depth.iter_mut().zip(d) {
            *acc += v * ds;
        }
    }
    depth
}

/// Spectral transmittance from the viewer `height` metres up to space along `dir`; zero when the ground is in the
/// way.
fn exact_transmittance(medium: &Medium, dir: Vec3f, height: f64) -> Spectrum {
    let length = dir.length();
    if length.is_nan() || length <= 0.0 {
        return [0.0; BANDS];
    }
    let mu = (dir.y / length).clamp(-1.0, 1.0) as f64;
    match column_depth(medium, PLANET_RADIUS + height, mu, EXACT_STEPS) {
        Some(column) => medium.extinction(column).map(|e| (-e).exp()),
        None => [0.0; BANDS],
    }
}

/// Spectral transmittance from the viewer to space along the ray seen at apparent elevation `e` (radians), bent on
/// its way as the air bends it (Bouguer): its curvature is the refractivity's fall with height across it, REFRACTIVITY
/// at the ground over the air's scale height, times the air's density. Near the horizon the bent ray stays lower than
/// the straight one along `e`, so it crosses more air, and the light seen there is that of the sun's lower limb, which
/// the straight ray along its true direction (below the horizon, into the ground) cannot carry: a sun on the horizon
/// crosses about 38 air masses (Kasten & Young 1989). Zero when the ray meets the ground. Away from the horizon it is
/// `exact_transmittance` along the same direction to a few parts in ten thousand.
fn refracted_transmittance(medium: &Medium, e: f64) -> Spectrum {
    refracted_transmittance_from(medium, e, VIEWER_HEIGHT)
}

/// [`refracted_transmittance`] from `height` metres above the ground (the viewer's own height or more; the far
/// ground's own, 0, for its light, `aerial::ground_light`).
fn refracted_transmittance_from(medium: &Medium, e: f64, height: f64) -> Spectrum {
    const STEPS: usize = 128;
    let r0 = PLANET_RADIUS + height.max(0.0);
    // the bent ray is a little longer than the straight one to the top
    let length = 1.1 * distance_to_top(r0, e.sin().clamp(-1.0, 1.0));
    let (mut p, mut v) = ([0.0f64, r0], [e.cos(), e.sin()]);
    let mut depth = [0.0f64; 4];
    for i in 0..STEPS {
        let (_, ds) = segment(i, STEPS, length);
        let q = [p[0] + v[0] * 0.5 * ds, p[1] + v[1] * 0.5 * ds];
        let r = (q[0] * q[0] + q[1] * q[1]).sqrt();
        if r < PLANET_RADIUS {
            return [0.0; BANDS];
        }
        if r > TOP_RADIUS {
            break;
        }
        let d = medium.densities_over(r, (q[0] * v[0] + q[1] * v[1]) / r, ds, true);
        for (acc, x) in depth.iter_mut().zip(d) {
            *acc += x * ds;
        }
        // Turn toward the denser air below: the refractive index's gradient across the ray.
        let up = [q[0] / r, q[1] / r];
        let along = up[0] * v[0] + up[1] * v[1];
        let g = REFRACTIVITY / RAYLEIGH_SCALE_HEIGHT * d[0] * ds;
        v = [v[0] - g * (up[0] - along * v[0]), v[1] - g * (up[1] - along * v[1])];
        let n = (v[0] * v[0] + v[1] * v[1]).sqrt();
        v = [v[0] / n, v[1] / n];
        p = [q[0] + v[0] * 0.5 * ds, q[1] + v[1] * 0.5 * ds];
    }
    medium.extinction(depth).map(|x| (-x).exp())
}

/// How far the air's refraction lifts what is seen at apparent elevation `e` (radians), in radians: Bennett's formula
/// (1982, Journal of Navigation 35, 255), R = cot(h + 7.31 / (h + 4.4)) arcminutes with h the apparent elevation in
/// degrees, for 1010 hPa and 10 C: 34.5' on the horizon, 9.9' at 5 deg, 1.7' at 30 deg, 0 at the zenith. Below the
/// horizon, where nothing is seen and the formula runs away, it holds the horizon's value.
pub fn refraction(e: f32) -> f32 {
    let h = e.to_degrees().clamp(0.0, 90.0);
    let arcmin = 1.0 / (h + 7.31 / (h + 4.4)).to_radians().tan();
    (arcmin.max(0.0) / 60.0).to_radians()
}

/// The apparent elevation of what is at true elevation `e` (radians): `refraction`'s inverse, a + refraction(a) = e
/// solved by fixed point (refraction's slope stays under a quarter, so eight steps leave a part in 10^5).
pub fn apparent_elevation(e: f32) -> f32 {
    let mut a = e;
    for _ in 0..8 {
        a = e + refraction(a);
    }
    a
}

/// `v` as a unit vector that `normalize` leaves bit for bit as it is, nudged by at most one unit in the last place per
/// component when it must be: the clouds' layers normalise the sun's direction they are given and a cloud stack
/// normalises it once more before its layers do, and their noise is read along it, so a direction normalising does not
/// settle would light the same layer two ways (1e-5 apart).
fn settled_unit(v: Vec3f) -> Vec3f {
    let v = v.normalize();
    if v.normalize() == v {
        return v;
    }
    let nudge = |x: f32, k: i32| if k == 0 || x == 0.0 { x } else { f32::from_bits((x.to_bits() as i32 + k) as u32) };
    for kx in -1..=1 {
        for ky in -1..=1 {
            for kz in -1..=1 {
                let c = vec3f(nudge(v.x, kx), nudge(v.y, ky), nudge(v.z, kz));
                if c.normalize() == c {
                    return c;
                }
            }
        }
    }
    v
}

/// Unit `d` (unit length) turned in its own vertical plane to elevation `e` (radians), its azimuth kept; straight
/// up or down stays as it is.
fn at_elevation(d: Vec3f, e: f32) -> Vec3f {
    let h = (d.x * d.x + d.z * d.z).sqrt();
    if !(h > 1.0e-6) {
        return d;
    }
    let (s, c) = e.sin_cos();
    vec3f(d.x / h * c, s, d.z / h * c)
}

/// Where what is seen along unit `d` truly is: `d` lowered by the refraction at its elevation.
pub fn unrefracted(d: Vec3f) -> Vec3f {
    let e = d.y.clamp(-1.0, 1.0).asin();
    at_elevation(d, e - refraction(e))
}

/// Each band's transmittance toward space over (height, angle above
/// grazing). Stored as transmittance, not optical depth, so a lookup is a
/// blend with no exponential: the sky-view march makes over a million.
struct DepthTable {
    transmittance: Vec<[f32; BANDS]>,
    /// A boundary layer's even haze (`Medium::haze_top`), left out of the table and taken along each lookup's own
    /// ray (`lookup`): the radius of its top and its extinction per metre per band. Its sharp top puts a cusp in the
    /// transmittance over height and over angle (a ray from just under the top toward a low sun runs inside the layer
    /// for a length that grows as the root of its depth under the top), which the table's bilinear steps blurred to
    /// several times the light near a setting sun.
    layer: Option<(f64, Spectrum)>,
}

impl DepthTable {
    fn new(medium: &Medium) -> DepthTable {
        let mut transmittance = Vec::with_capacity(DEPTH_HEIGHTS * DEPTH_ANGLES);
        for j in 0..DEPTH_HEIGHTS {
            let y = j as f64 / (DEPTH_HEIGHTS - 1) as f64;
            let r = PLANET_RADIUS + (TOP_RADIUS - PLANET_RADIUS) * y * y;
            let mu_graze = grazing_mu(r);
            for i in 0..DEPTH_ANGLES {
                let x = i as f64 / (DEPTH_ANGLES - 1) as f64;
                let mu = mu_graze + (1.0 - mu_graze) * x * x;
                // Every column is at or above the grazing ray by construction.
                let ext = medium.extinction(integrate_column(medium, r, mu, DEPTH_STEPS, false));
                transmittance.push(ext.map(|e| (-e).exp() as f32));
            }
        }
        let layer = medium.haze_top.map(|top| {
            (PLANET_RADIUS + top, std::array::from_fn(|k| (1.0 - FREE_HAZE_SHARE) * medium.mie_extinction[k]))
        });
        DepthTable { transmittance, layer }
    }

    /// Transmittance toward space from radius `r` along `mu`. The planet's
    /// shadow is decided exactly, not blurred by the table.
    fn transmittance(&self, r: f64, mu: f64) -> Spectrum {
        self.transmittance_at(&DepthHeight::new(r), mu)
    }

    /// `transmittance` at a height whose part of the lookup is done: a march
    /// asks once per step for every sun direction it lights the step with.
    /// The sun is a disc, not a point: near the planet's limb as seen from
    /// here the limb hides part of it, and what arrives is its visible face's
    /// light, each part through the air its own ray crosses (`penumbra`), so
    /// the Earth's shadow on the air has a soft edge. With the disc's centre
    /// PENUMBRA_REACH radii above the limb or more it is the centre's lookup,
    /// as it always was; with all of it under the limb, 0.
    fn transmittance_at(&self, h: &DepthHeight, mu: f64) -> Spectrum {
        // Above the dense air (the mesosphere) the sun's ray re-enters it, and the light is the table's from there,
        // the penumbra included.
        if h.r > TOP_RADIUS {
            return self.transmittance_from_mesosphere(h.r, mu);
        }
        if mu >= h.mu_clear {
            return self.lookup(h, mu);
        }
        if mu <= h.mu_hidden {
            return [0.0; BANDS];
        }
        self.penumbra(h, mu)
    }

    /// The light of the sun's disc whose centre is at cosine `mu`, near the limb (`transmittance_at`): the share of
    /// its light above the limb (`disc_light_below`) times the mean transmittance over its visible face, from
    /// PENUMBRA_NODES rows of equal height, each weighted by the disc's light across it (`disc_row_light`) and lit
    /// along its own direction. From PENUMBRA_BLEND radii above the limb the face's mean hands over to the centre's
    /// lookup, which it is at PENUMBRA_REACH.
    fn penumbra(&self, h: &DepthHeight, mu: f64) -> Spectrum {
        let theta = mu.clamp(-1.0, 1.0).acos();
        // The disc's centre above the limb, in disc radii, and the lowest row of it the limb leaves in sight (from
        // its centre, up, in radii).
        let above = (h.mu_graze.acos() - theta) / SUN_ANGULAR_RADIUS;
        let lowest = (-above).clamp(-1.0, 1.0);
        let seen = 1.0 - disc_light_below(lowest);
        let (mut sum, mut weight) = ([0.0f64; BANDS], 0.0f64);
        for i in 0..PENUMBRA_NODES {
            let y = lowest + (1.0 - lowest) * (i as f64 + 0.5) / PENUMBRA_NODES as f64;
            let w = disc_row_light(y);
            let t = self.lookup(h, (theta - y * SUN_ANGULAR_RADIUS).cos());
            for k in 0..BANDS {
                sum[k] += w * t[k];
            }
            weight += w;
        }
        let face: Spectrum = std::array::from_fn(|k| if weight > 0.0 { sum[k] / weight * seen } else { 0.0 });
        if above <= PENUMBRA_BLEND {
            return face;
        }
        let centre = self.lookup(h, mu);
        let s = ((above - PENUMBRA_BLEND) / (PENUMBRA_REACH - PENUMBRA_BLEND)).clamp(0.0, 1.0);
        let s = s * s * (3.0 - 2.0 * s);
        std::array::from_fn(|k| face[k] + (centre[k] - face[k]) * s)
    }

    /// The table's lookup along `mu`, at or above the limb: bilinear over (height, angle above grazing), times a
    /// boundary layer's transmittance over the ray's own chord through it (`layer`).
    fn lookup(&self, h: &DepthHeight, mu: f64) -> Spectrum {
        let x = ((mu - h.mu_graze) * h.inv_span).clamp(0.0, 1.0).sqrt();
        let fx = x * (DEPTH_ANGLES - 1) as f64;
        let x0 = (fx as usize).min(DEPTH_ANGLES - 2);
        let (ty, tx) = (h.ty, fx - x0 as f64);
        let row0 = h.row;
        let row1 = row0 + DEPTH_ANGLES;
        let (a, b) = (&self.transmittance[row0 + x0], &self.transmittance[row0 + x0 + 1]);
        let (c, d) = (&self.transmittance[row1 + x0], &self.transmittance[row1 + x0 + 1]);
        let t: Spectrum = std::array::from_fn(|k| {
            let near = a[k] as f64 + (b[k] as f64 - a[k] as f64) * tx;
            let far = c[k] as f64 + (d[k] as f64 - c[k] as f64) * tx;
            near + (far - near) * ty
        });
        match self.layer {
            Some((top, ext)) => {
                let chord = chord_inside(h.r, mu, top);
                if chord > 0.0 {
                    std::array::from_fn(|k| t[k] * (-ext[k] * chord).exp())
                } else {
                    t
                }
            }
            None => t,
        }
    }

    /// Transmittance toward space from radius `r` in the mesosphere (above the dense air) along `mu`: the mesosphere
    /// dims nothing, so it is 1 unless the ray dips back into the dense air, and then the table's from where it enters
    /// it (a straight ray keeps r x its sine).
    fn transmittance_from_mesosphere(&self, r: f64, mu: f64) -> Spectrum {
        let b2 = r * r * (1.0 - mu * mu);
        if mu >= 0.0 || b2 >= TOP_RADIUS * TOP_RADIUS {
            return [1.0; BANDS];
        }
        let mu_in = -(1.0 - b2 / (TOP_RADIUS * TOP_RADIUS)).max(0.0).sqrt();
        self.transmittance(TOP_RADIUS, mu_in)
    }
}

/// The length of the ray from radius `r` along cosine `mu` (to the vertical there) that lies inside the sphere of
/// radius `rb`, from its start on: the chord `share_inside` cuts, for the half line t >= 0.
fn chord_inside(r: f64, mu: f64, rb: f64) -> f64 {
    let q2 = (rb - r) * (rb + r) + (r * mu) * (r * mu);
    if !(q2 > 0.0) {
        return 0.0;
    }
    let q = q2.sqrt();
    (q - r * mu - (-q - r * mu).max(0.0)).max(0.0)
}

/// The height's share of a `DepthTable` lookup.
#[derive(Clone, Copy, Default)]
struct DepthHeight {
    r: f64,
    mu_graze: f64,
    inv_span: f64,
    /// The first texel of the lower row, and the weight of the upper one.
    row: usize,
    ty: f64,
    /// The sun's cosine at and above which its disc's centre is PENUMBRA_REACH radii or more above the limb (the
    /// lookup is the centre's), and at and below which the limb hides all of the disc.
    mu_clear: f64,
    mu_hidden: f64,
}

impl DepthHeight {
    fn new(r: f64) -> DepthHeight {
        let y = ((r - PLANET_RADIUS) / (TOP_RADIUS - PLANET_RADIUS)).clamp(0.0, 1.0).sqrt();
        let mu_graze = grazing_mu(r);
        let fy = y * (DEPTH_HEIGHTS - 1) as f64;
        let y0 = (fy as usize).min(DEPTH_HEIGHTS - 2);
        // The limb's zenith angle g: cos(g -+ x) = cos g cos x +- sin g sin x.
        let sin_graze = (1.0 - mu_graze * mu_graze).max(0.0).sqrt();
        let (reach_sin, reach_cos) = (PENUMBRA_REACH * SUN_ANGULAR_RADIUS).sin_cos();
        let (radius_sin, radius_cos) = SUN_ANGULAR_RADIUS.sin_cos();
        DepthHeight {
            r,
            mu_graze,
            inv_span: 1.0 / (1.0 - mu_graze),
            row: y0 * DEPTH_ANGLES,
            ty: fy - y0 as f64,
            mu_clear: mu_graze * reach_cos + sin_graze * reach_sin,
            mu_hidden: mu_graze * radius_cos - sin_graze * radius_sin,
        }
    }
}

/// The light of the sun's disc (radius 1, a linear limb darkening PENUMBRA_LIMB) across its row at height `y` (-1..1
/// from its centre): over the half chord s, 2 (1 - u) s + u (pi / 2) s^2.
fn disc_row_light(y: f64) -> f64 {
    let s2 = (1.0 - y * y).max(0.0);
    2.0 * (1.0 - PENUMBRA_LIMB) * s2.sqrt() + PENUMBRA_LIMB * std::f64::consts::FRAC_PI_2 * s2
}

/// The share of the disc's light (`disc_row_light`) below its row at height `y` (-1..1 from its centre): the row
/// light's integral, over the whole disc's pi (1 - u / 3).
fn disc_light_below(y: f64) -> f64 {
    let y = y.clamp(-1.0, 1.0);
    let s = (1.0 - y * y).max(0.0).sqrt();
    let (u, half_pi) = (PENUMBRA_LIMB, std::f64::consts::FRAC_PI_2);
    let below = (1.0 - u) * (y * s + y.asin() + half_pi) + u * half_pi * (y - y * y * y / 3.0 + 2.0 / 3.0);
    (below / (std::f64::consts::PI * (1.0 - u / 3.0))).clamp(0.0, 1.0)
}

fn rayleigh_phase(nu: f64) -> f64 {
    3.0 / (16.0 * std::f64::consts::PI) * (1.0 + nu * nu)
}

/// The low haze's kind (`AtmosphereParams::aerosol`): how its extinction falls with wavelength (the Angstrom exponent),
/// how much of what it takes out it scatters (its single-scattering albedo, per band), and its phase: a broad forward
/// lobe, a narrow peak round the sun (its diffraction) and, for all but the calibrated continental haze, a small
/// backward lobe (real particles' phase rises again toward the antisolar point, which lights the Belt of Venus).
/// The optical depth is the Haze control's for every kind. The stratosphere's thin haze is never of the kind: it
/// keeps the continental's (`STRAT_DEPTH`). The kinds' numbers are AERONET's climatology (Dubovik et
/// al. 2002, "Variability of absorption and optical properties of key aerosol types observed in worldwide
/// locations", J. Atmos. Sci. 59): urban-industrial absorbs (albedo 0.85-0.9), sea salt barely (0.98) and has a low
/// Angstrom exponent (0.3-0.8), desert dust absorbs blue (0.92 at 440 nm, 0.97 at 670 nm, its iron oxides) with an
/// exponent near 0.2-0.5, and the larger particles of salt and dust scatter further forward (g 0.72-0.75).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Aerosol {
    angstrom: f64,
    albedo: Spectrum,
    g_broad: f64,
    g_peak: f64,
    peak_share: f64,
    g_back: f64,
    back_share: f64,
}

impl Aerosol {
    fn named(name: &str) -> Aerosol {
        let flat = |a: f64| [a; BANDS];
        match name {
            "maritime" => Aerosol { angstrom: 0.5, albedo: flat(0.985), g_broad: 0.72, g_peak: 0.96, peak_share: 0.3, g_back: -0.4, back_share: 0.04 },
            "urban" => Aerosol { angstrom: 1.6, albedo: flat(0.86), g_broad: 0.62, g_peak: 0.95, peak_share: 0.2, g_back: -0.3, back_share: 0.03 },
            // (400..680 nm in 40 nm bands: the dust's iron oxides take the blue)
            "desert" => Aerosol {
                angstrom: 0.3,
                albedo: [0.88, 0.91, 0.935, 0.95, 0.96, 0.967, 0.97, 0.972],
                g_broad: 0.73,
                g_peak: 0.96,
                peak_share: 0.3,
                g_back: -0.4,
                back_share: 0.05,
            },
            // continental: the haze the sky was calibrated with, unchanged
            _ => Aerosol {
                angstrom: AEROSOL_ANGSTROM,
                albedo: flat(AEROSOL_ALBEDO),
                g_broad: MIE_G_BROAD,
                g_peak: MIE_G_PEAK,
                peak_share: MIE_PEAK_SHARE,
                g_back: 0.0,
                back_share: 0.0,
            },
        }
    }

    /// The haze's phase at view-sun cosine `nu`: the broad lobe, the narrow peak and the backward lobe.
    fn phase(&self, nu: f64) -> f64 {
        let front = (1.0 - self.peak_share - self.back_share) * cornette_shanks(nu, self.g_broad)
            + self.peak_share * cornette_shanks(nu, self.g_peak);
        if self.back_share > 0.0 {
            front + self.back_share * cornette_shanks(nu, self.g_back)
        } else {
            front
        }
    }
}

/// Cornette-Shanks phase (as in glsl-atmosphere): Henyey-Greenstein with a
/// (1 + nu^2) factor that fits haze better; it integrates to 1 over the sphere.
fn cornette_shanks(nu: f64, g: f64) -> f64 {
    let gg = g * g;
    3.0 / (8.0 * std::f64::consts::PI) * ((1.0 - gg) * (1.0 + nu * nu))
        / ((2.0 + gg) * (1.0 + gg - 2.0 * g * nu).powf(1.5))
}

/// The multiple-scattering source (Hillaire 2020, section 5.5): the radiance
/// of all orders above the first, averaged over directions, per unit of the
/// sun's light above the air, over (height, cosine of the sun's zenith
/// angle). A view sample adds scattering x this.
struct MultiScatter {
    /// MS_HEIGHTS rows of MS_READ_ANGLES sun angles, evenly over the sun's cosine from `mu0` to `mu1`.
    psi: Vec<Spectrum>,
    /// The sun's cosine at the first and the last column.
    mu0: f64,
    mu1: f64,
}

/// One step of a march whose geometry does not depend on the sun: where it
/// is, and what it scatters toward the ray's origin per unit of light (air,
/// the low haze and the stratosphere's apart, with the view transmittance and
/// the step length in).
#[derive(Clone, Copy)]
struct RayStep {
    /// Position, the ray's origin at (0, r0, 0) and y up.
    p: [f64; 3],
    r: f64,
    air: Spectrum,
    haze: Spectrum,
    strat: Spectrum,
    /// The step's height in the depth table and in the multiple-scattering one.
    depth: DepthHeight,
    ms_row: usize,
    ms_ty: f64,
}

impl Default for RayStep {
    fn default() -> RayStep {
        RayStep { p: [0.0; 3], r: 0.0, air: [0.0; BANDS], haze: [0.0; BANDS], strat: [0.0; BANDS], depth: DepthHeight::default(), ms_row: 0, ms_ty: 0.0 }
    }
}

impl MultiScatter {
    fn new(medium: &Medium, table: &DepthTable, sun_sin: f64) -> MultiScatter {
        let sun_theta = sun_sin.clamp(-1.0, 1.0).acos();
        let reach = MS_ANGLE_REACH_DEG.to_radians();
        let mu0 = (sun_theta + reach).min(std::f64::consts::PI).cos();
        let mu1 = (sun_theta - reach).max(0.0).cos();
        let dirs = sphere_directions(MS_DIRS);
        let inv_4pi = 1.0 / (4.0 * std::f64::consts::PI);
        let n = dirs.len() as f64;
        // The point sits at (0, r, 0) and the sun in the x-y plane.
        let suns: Vec<[f64; 2]> = (0..MS_ANGLES)
            .map(|i| {
                let mu = mu0 + (mu1 - mu0) * i as f64 / (MS_ANGLES - 1) as f64;
                [(1.0 - mu * mu).max(0.0).sqrt(), mu]
            })
            .collect();
        // The deep columns' share of MS_DEEP_DIRS directions (see MS_DEEP_DIRS).
        let deep: Vec<f64> = suns.iter().map(|s| deep_share(s[1])).collect();
        let deep_dirs = if deep.iter().any(|&d| d > 0.0) { sphere_directions(MS_DEEP_DIRS) } else { Vec::new() };
        let mut psi = vec![[0.0f64; BANDS]; MS_HEIGHTS * MS_ANGLES];
        let mut steps = [RayStep::default(); MS_STEPS + MESOSPHERE_STEPS];
        let mut second = vec![[0.0f64; BANDS]; MS_ANGLES];
        let mut second_deep = vec![[0.0f64; BANDS]; MS_ANGLES];
        for j in 0..MS_HEIGHTS {
            let y = j as f64 / (MS_HEIGHTS - 1) as f64;
            let r = PLANET_RADIUS + (TOP_RADIUS - PLANET_RADIUS) * y * y;
            second.iter_mut().for_each(|l| *l = [0.0; BANDS]);
            second_deep.iter_mut().for_each(|l| *l = [0.0; BANDS]);
            let mut transfer = [0.0f64; BANDS];
            let mut deep_transfer = [0.0f64; BANDS];
            // MS_DIRS directions for every column, then MS_DEEP_DIRS for the deep ones (the transfer, smooth over
            // directions, stays the MS_DIRS directions' own).
            for (w, deep_dir) in dirs.iter().map(|w| (w, false)).chain(deep_dirs.iter().map(|w| (w, true))) {
                let (second, transfer) = if deep_dir { (&mut second_deep, &mut deep_transfer) } else { (&mut second, &mut transfer) };
                let ground = hits_ground(r, w[1]);
                let length = if ground { distance_to_ground(r, w[1]) } else { distance_to_top(r, w[1]) };
                // A ray that leaves the dense air goes on through the mesosphere.
                let (count, beyond) =
                    if ground { (MS_STEPS, 0.0) } else { (MS_STEPS + MESOSPHERE_STEPS, distance_to_mesosphere_top(r, w[1]) - length) };
                let steps = &mut steps[..count];
                // The march is the same for every sun angle; only the light
                // that reaches each step differs.
                let mut depth = [0.0f64; BANDS];
                for (s, step) in steps.iter_mut().enumerate() {
                    let (t, ds) = march_segment(s, MS_STEPS, length, beyond);
                    let p = [w[0] * t, r + w[1] * t, w[2] * t];
                    let rp = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
                    let dens = medium.densities_over(rp, (r * w[1] + t) / rp, ds, true);
                    let ext = medium.extinction(dens);
                    let sca = medium.scattering(dens);
                    let mut weight = [0.0f64; BANDS];
                    for k in 0..BANDS {
                        let view_t = (-(depth[k] + 0.5 * ext[k] * ds)).exp();
                        depth[k] += ext[k] * ds;
                        weight[k] = view_t * sca[k] * ds;
                        transfer[k] += weight[k];
                    }
                    *step = RayStep { p, r: rp, air: weight, depth: DepthHeight::new(rp), ..RayStep::default() };
                }
                let ground_at = ground.then(|| {
                    let p = [w[0] * length, r + w[1] * length, w[2] * length];
                    let rp = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
                    let t: Spectrum = depth.map(|d| (-d).exp());
                    (p, rp, t)
                });
                for ((sun, acc), share) in suns.iter().zip(second.iter_mut()).zip(&deep) {
                    if deep_dir && *share == 0.0 {
                        continue;
                    }
                    for step in steps.iter() {
                        let mu_sun = (step.p[0] * sun[0] + step.p[1] * sun[1]) / step.r;
                        let sun_t = table.transmittance_at(&step.depth, mu_sun);
                        // The first scattering is taken as isotropic, as in the paper.
                        for k in 0..BANDS {
                            acc[k] += step.air[k] * sun_t[k] * inv_4pi;
                        }
                    }
                    if let Some((p, rp, view_t)) = ground_at {
                        // The lit ground: Lambertian, its albedo x E cos / pi.
                        let mu_sun = (p[0] * sun[0] + p[1] * sun[1]) / rp;
                        if mu_sun > 0.0 {
                            let sun_t = table.transmittance(PLANET_RADIUS, mu_sun);
                            for k in 0..BANDS {
                                acc[k] += view_t[k] * sun_t[k] * mu_sun * medium.ground[k] / std::f64::consts::PI;
                            }
                        }
                    }
                }
            }
            // Every order above the second is the one before it times the
            // share an isotropic radiance scatters again: 1 / (1 - f).
            let f: Spectrum = std::array::from_fn(|k| (transfer[k] / n).min(0.99));
            for (i, l) in second.iter().enumerate() {
                psi[j * MS_ANGLES + i] = if deep[i] > 0.0 {
                    let (d, share, n_deep) = (&second_deep[i], deep[i], MS_DEEP_DIRS as f64);
                    std::array::from_fn(|k| ((1.0 - share) * l[k] / n + share * d[k] / n_deep) / (1.0 - f[k]))
                } else {
                    std::array::from_fn(|k| l[k] / n / (1.0 - f[k]))
                };
            }
        }
        MultiScatter { psi: read_columns(&psi), mu0, mu1 }
    }

    /// The height's share of a lookup at radius `r`: the first texel of the
    /// lower row and the weight of the upper one.
    fn height(r: f64) -> (usize, f64) {
        let y = ((r - PLANET_RADIUS) / (TOP_RADIUS - PLANET_RADIUS)).clamp(0.0, 1.0).sqrt();
        let fy = y * (MS_HEIGHTS - 1) as f64;
        let y0 = (fy as usize).min(MS_HEIGHTS - 2);
        (y0 * MS_READ_ANGLES, fy - y0 as f64)
    }

    /// The source at the height `height` names, with the sun at cosine
    /// `mu_sun` to the vertical there.
    fn at(&self, (row0, ty): (usize, f64), mu_sun: f64) -> Spectrum {
        let x = if self.mu1 > self.mu0 { ((mu_sun - self.mu0) / (self.mu1 - self.mu0)).clamp(0.0, 1.0) } else { 0.0 };
        let fx = x * (MS_READ_ANGLES - 1) as f64;
        let x0 = (fx as usize).min(MS_READ_ANGLES - 2);
        let tx = fx - x0 as f64;
        let row1 = row0 + MS_READ_ANGLES;
        let (a, b) = (&self.psi[row0 + x0], &self.psi[row0 + x0 + 1]);
        let (c, d) = (&self.psi[row1 + x0], &self.psi[row1 + x0 + 1]);
        std::array::from_fn(|k| {
            let near = a[k] + (b[k] - a[k]) * tx;
            let far = c[k] + (d[k] - c[k]) * tx;
            near + (far - near) * ty
        })
    }
}

/// How much of a multiple-scattering column takes the deep direction set (MS_DEEP_DIRS) for its sun at cosine
/// `mu_sun` to the point's vertical: 0 down to MS_DEEP_FROM_DEG below the point's horizon, 1 from MS_DEEP_TO_DEG down.
fn deep_share(mu_sun: f64) -> f64 {
    let (from, to) = (MS_DEEP_FROM_DEG.to_radians().sin(), MS_DEEP_TO_DEG.to_radians().sin());
    let t = ((-mu_sun - from) / (to - from)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The multiple-scattering table `psi` as it is worked out (MS_HEIGHTS rows of MS_ANGLES sun angles, evenly over the
/// sun's cosine) read on MS_READ_ANGLES columns over the same span: each band's logarithm (over PSI_FLOOR) through the
/// worked-out columns, a monotone cubic between them (Fritsch & Butland 1984: each column's slope the harmonic mean of
/// its two secants, which keeps the curve within its neighbours, and a column next to one nothing lights on its own
/// side's slope). A source that falls by a constant factor per degree (twilight's, about 3x) is read as it is, where
/// the linear interpolation of two columns 2 degrees apart read it up to 1.75x too bright midway; above the point's
/// horizon, where the logarithm bends over, the cubic follows it (a straight line through the logarithm read it 3-9 %
/// too dark from +3 to -3 deg).
fn read_columns(psi: &[Spectrum]) -> Vec<Spectrum> {
    let (angles, read) = (MS_ANGLES, MS_READ_ANGLES);
    let mut out = vec![[0.0f64; BANDS]; MS_HEIGHTS * read];
    for (j, row) in psi.chunks(angles).enumerate() {
        for k in 0..BANDS {
            let y: Vec<f64> = row.iter().map(|l| l[k].max(PSI_FLOOR).ln()).collect();
            let d: Vec<f64> = y.windows(2).map(|w| w[1] - w[0]).collect();
            let m: Vec<f64> = (0..angles)
                .map(|i| {
                    if i == 0 {
                        d[0]
                    } else if i == angles - 1 {
                        d[angles - 2]
                    } else if d[i - 1] * d[i] <= 0.0 {
                        0.0
                    } else {
                        2.0 * d[i - 1] * d[i] / (d[i - 1] + d[i])
                    }
                })
                .collect();
            for c in 0..read {
                let x = c as f64 / (read - 1) as f64 * (angles - 1) as f64;
                let i = (x as usize).min(angles - 2);
                let t = x - i as f64;
                // the cubic Hermite basis
                let (t2, t3) = (t * t, t * t * t);
                let (h0, h1) = (2.0 * t3 - 3.0 * t2 + 1.0, 3.0 * t2 - 2.0 * t3);
                let v = h0 * y[i] + h1 * y[i + 1] + (t3 - 2.0 * t2 + t) * m[i] + (t3 - t2) * m[i + 1];
                out[j * read + c][k] = v.exp();
            }
        }
    }
    out
}

/// `n` directions spread evenly over the sphere (a spherical Fibonacci set).
fn sphere_directions(n: usize) -> Vec<[f64; 3]> {
    let golden = std::f64::consts::PI * (3.0 - 5.0f64.sqrt());
    (0..n)
        .map(|i| {
            let y = 1.0 - 2.0 * (i as f64 + 0.5) / n as f64;
            let ring = (1.0 - y * y).max(0.0).sqrt();
            let phi = golden * i as f64;
            [ring * phi.cos(), y, ring * phi.sin()]
        })
        .collect()
}

/// The view march along a ray at elevation sine `view_sin`: VIEW_STEPS steps through the dense air, then
/// MESOSPHERE_STEPS through the mesosphere above it. It is the same for every azimuth, so a row of the sky-view table
/// marches once.
fn view_steps(medium: &Medium, view_sin: f64) -> [RayStep; VIEW_STEPS + MESOSPHERE_STEPS] {
    let r0 = PLANET_RADIUS + VIEWER_HEIGHT;
    let length = distance_to_top(r0, view_sin);
    let beyond = distance_to_mesosphere_top(r0, view_sin) - length;
    let mut steps = [RayStep::default(); VIEW_STEPS + MESOSPHERE_STEPS];
    // Extinction optical depth from the viewer to the start of the current segment.
    let mut depth = [0.0f64; BANDS];
    for (i, step) in steps.iter_mut().enumerate() {
        let (t, ds) = march_segment(i, VIEW_STEPS, length, beyond);
        *step = view_step(medium, r0, view_sin, t, ds, &mut depth);
    }
    steps
}

/// One step of a view march from radius `r0` at elevation sine `view_sin`: its middle `t` metres along the ray, `ds`
/// long, weighed at its middle, the optical depth from the viewer to the step's start in `depth` (carried past it). The
/// haze is read over the step (`Medium::densities_over`: a boundary layer's top cuts it exactly). The step's light
/// is split as `in_scatter` sums it: the air's, the low haze's and the stratosphere's, each attenuated to the viewer.
/// `aerial`'s marches take their steps from here, so a viewer at a height of its own sees the same air.
fn view_step(medium: &Medium, r0: f64, view_sin: f64, t: f64, ds: f64, depth: &mut Spectrum) -> RayStep {
    let r = radius_at(r0, view_sin, t);
    let dens = medium.densities_over(r, (r0 * view_sin + t) / r, ds, true);
    let ext = medium.extinction(dens);
    let haze_sca = medium.haze_scattering(dens);
    let (mut air, mut haze, mut strat) = ([0.0f64; BANDS], [0.0f64; BANDS], [0.0f64; BANDS]);
    for k in 0..BANDS {
        let view_t = (-(depth[k] + 0.5 * ext[k] * ds)).exp();
        depth[k] += ext[k] * ds;
        air[k] = view_t * medium.rayleigh[k] * dens[0] * ds;
        haze[k] = view_t * haze_sca[k] * ds;
        strat[k] = view_t * medium.strat_scattering[k] * dens[3] * ds;
    }
    // `p` holds (t, 0, 0): the distance along the ray is all `in_scatter` needs.
    let (ms_row, ms_ty) = MultiScatter::height(r);
    RayStep { p: [t, 0.0, 0.0], r, air, haze, strat, depth: DepthHeight::new(r), ms_row, ms_ty }
}

/// `compress_blue` leaves a light whose blue lies BLUE_THRESHOLD of the way or less from its largest channel down to
/// zero as it is, and draws the blue of those beyond it in along the reference gamut compression's curve (Academy,
/// ACES 1.3, 2021, "Reference Gamut Compression": its yellow threshold and power), so the farthest light the bands can
/// carry (`blue_limit`) lands on the gamut's edge and none stays outside. A camera records light outside its gamut that
/// way, inside with a little blue, not cut to none: the photos' 1 degree sun and its aureole keep 2.6-4.2 % of their
/// red (`Medium::light_rgb`), the curve gives the model's 1 degree sun 2.9 %. The calibrated sky stays as it was: from
/// 85 to 7 degrees the haze's and the air's light keep their blue over 15 % of their red (at most 0.85 of the way; the
/// sun at 7 degrees 0.79), and only the stratosphere's faint light along the horizon, reddened across the whole air, is
/// drawn in (the sky moves by under 0.01 % in luminance, 0.34 % in blue).
const BLUE_THRESHOLD: f32 = 0.88;
const BLUE_POWER: f32 = 1.2;

/// How far below zero, in parts of its largest channel, the blue of the farthest light the bands can carry falls (the
/// distance `compress_blue` measures, past 1). The distance divides by the largest channel, so it is not linear in the
/// spectrum and a mix lies farther out than any band alone: band 560 alone 1.130, bands 560 and 600 at 1 : 0.484 (red
/// and green equal) 1.145. With the largest channel held at 1 the deepest blue is a linear programme over the bands'
/// amounts (red and green each at most 1), whose optimum lies on a corner: one band with its largest channel at 1, or
/// two with red and green both at 1. The farthest corner is the limit; no light the bands carry lies beyond it.
fn blue_limit(to_rgb: &[Spectrum; 3]) -> f32 {
    let [r, g, b] = to_rgb;
    let mut limit = 1.0f64;
    for j in 0..BANDS {
        let m = r[j].max(g[j]).max(b[j]);
        if m > 0.0 {
            limit = limit.max((m - b[j]) / m);
        }
        for k in j + 1..BANDS {
            // the amounts of bands j and k that put red and green both at 1
            let det = r[j] * g[k] - r[k] * g[j];
            if det == 0.0 {
                continue;
            }
            let (sj, sk) = ((g[k] - r[k]) / det, (r[j] - g[j]) / det);
            if sj >= 0.0 && sk >= 0.0 {
                limit = limit.max(1.0 - (b[j] * sj + b[k] * sk));
            }
        }
    }
    limit as f32
}

/// A light's colour with its blue drawn inside Rec.709: its distance from the largest channel, d = (max - b) / max (1
/// on the gamut's edge, past it outside), left alone up to BLUE_THRESHOLD and compressed beyond it so `limit` lands
/// on 1 (`BLUE_THRESHOLD`). A 1 degree sun's light (d 1.03) keeps a blue of 2.9 % of its red; red and green, and any
/// light whose blue is not short, are left as they are.
fn compress_blue(c: Vec3f, limit: f32) -> Vec3f {
    let m = c.x.max(c.y).max(c.z);
    if !(m > 0.0) {
        return c;
    }
    let d = (m - c.z) / m;
    let (t, p) = (BLUE_THRESHOLD, BLUE_POWER);
    if !(d > t) || !(limit > 1.0) {
        return c;
    }
    // the curve's scale, so that `limit` lands on the edge
    let scale = (limit - t) / (((1.0 - t) / (limit - t)).powf(-p) - 1.0).powf(1.0 / p);
    let x = (d - t) / scale;
    let d = t + scale * x / (1.0 + x.powf(p)).powf(1.0 / p);
    vec3f(c.x, c.y, m * (1.0 - d))
}

/// A colour held inside Rec.709: a channel that falls below a millionth of the colour's sum (a deeply reddened
/// spectrum's blue, through the matching functions' negative lobes) is held there, never exactly 0, which a light's
/// colour is divided by.
fn gamut_floor(c: Vec3f) -> Vec3f {
    let floor = 1.0e-6 * (c.x.max(0.0) + c.y.max(0.0) + c.z.max(0.0));
    vec3f(c.x.max(floor), c.y.max(floor), c.z.max(floor))
}

/// What reaches the viewer along a ray marched as `steps`, per unit of the sun's light above the air, with the sun
/// at elevation sine `sun_sin` and view-sun cosine `nu`, in four parts without the phase: the air's single
/// scattering, the low haze's, the stratosphere's haze's, and the multiple-scattering source (isotropic). The phase is
/// the same at every step of a ray (the sun is at infinity), so the radiance is air x rayleigh_phase(nu) + haze x the
/// aerosol's phase(nu) + strat x the continental's phase(nu) + multi. The parts still depend on `nu` through the sun's
/// transmittance at each step (the Earth's shadow), but smoothly.
fn in_scatter(table: &DepthTable, steps: &[RayStep], sun_sin: f64, nu: f64, multi: &MultiScatter) -> [Spectrum; 4] {
    let r0 = PLANET_RADIUS + VIEWER_HEIGHT;
    let (mut air, mut haze, mut strat, mut ms) = ([0.0f64; BANDS], [0.0f64; BANDS], [0.0f64; BANDS], [0.0f64; BANDS]);
    for step in steps {
        let (t, r) = (step.p[0], step.r);
        // The sun's direction is fixed but the local vertical turns along the
        // ray: mu_sun = dot(sample position, sun) / |sample position|.
        let mu_sun = (r0 * sun_sin + t * nu) / r;
        let sun_t = table.transmittance_at(&step.depth, mu_sun);
        let psi = multi.at((step.ms_row, step.ms_ty), mu_sun);
        for k in 0..BANDS {
            air[k] += sun_t[k] * step.air[k];
            haze[k] += sun_t[k] * step.haze[k];
            strat[k] += sun_t[k] * step.strat[k];
            ms[k] += (step.air[k] + step.haze[k] + step.strat[k]) * psi[k];
        }
    }
    [air, haze, strat, ms]
}

/// The sky-view table: LUT_HEIGHT rows of LUT_WIDTH texels (row 0 at the horizon), four planes without the phase
/// (`in_scatter`), each one light's colour in linear Rec.709 (`Medium::light_rgb`). A lookup reads the planes
/// bilinearly and applies the exact phase for its own direction, then the floor, so the glow round the sun, whose
/// narrow peak spans a few of the table's texels, stays sharp at any map width (the phase-free sky-view planes of
/// Physical Atmosphere 2, as an idea; our own code).
struct SkyLut {
    /// The low haze's kind, whose phase a lookup applies to its plane.
    aerosol: Aerosol,
    /// The kind whose phase the stratosphere's plane takes: the continental (sulphate) whatever the low haze's. None
    /// when the low haze is of that kind too, so one phase serves both planes.
    strat_aerosol: Option<Aerosol>,
    air: Vec<Vec3f>,
    haze: Vec<Vec3f>,
    strat: Vec<Vec3f>,
    multi: Vec<Vec3f>,
}

impl SkyLut {
    /// The sky at relative azimuth `phi` (0..pi from the sun) and elevation `elevation` (0..pi/2), the sun at
    /// elevation sine `sun_sin`, cosine `sun_cos`.
    fn at(&self, phi: f32, elevation: f32, sun_sin: f32, sun_cos: f32) -> Vec3f {
        let fx = (phi / std::f32::consts::PI).clamp(0.0, 1.0) * (LUT_WIDTH - 1) as f32;
        let fy = (elevation / std::f32::consts::FRAC_PI_2).clamp(0.0, 1.0).sqrt() * (LUT_HEIGHT - 1) as f32;
        let x0 = (fx as usize).min(LUT_WIDTH - 2);
        let y0 = (fy as usize).min(LUT_HEIGHT - 2);
        let tx = fx - x0 as f32;
        let ty = fy - y0 as f32;
        let row0 = y0 * LUT_WIDTH;
        let row1 = row0 + LUT_WIDTH;
        let read = |plane: &[Vec3f]| {
            let near = plane[row0 + x0].mix(plane[row0 + x0 + 1], tx);
            let far = plane[row1 + x0].mix(plane[row1 + x0 + 1], tx);
            near.mix(far, ty)
        };
        let el = elevation.clamp(0.0, std::f32::consts::FRAC_PI_2);
        let nu = (el.cos() * sun_cos * phi.cos() + el.sin() * sun_sin).clamp(-1.0, 1.0) as f64;
        let haze_phase = self.aerosol.phase(nu) as f32;
        let strat_phase = self.strat_aerosol.as_ref().map_or(haze_phase, |kind| kind.phase(nu) as f32);
        gamut_floor(read(&self.air) * rayleigh_phase(nu) as f32 + read(&self.haze) * haze_phase + read(&self.strat) * strat_phase + read(&self.multi))
    }

    /// Every plane times `k` (`Atmosphere::lifted`): the sky `at` reads is k times as bright, the phases and the gamut
    /// floor being linear in it.
    fn scale(&mut self, k: f32) {
        for plane in [&mut self.air, &mut self.haze, &mut self.strat, &mut self.multi] {
            plane.iter_mut().for_each(|texel| *texel *= k);
        }
    }
}

fn build_sky_lut(medium: &Medium, table: &DepthTable, sun_sin: f64, multi: &MultiScatter) -> SkyLut {
    let sun_cos = (1.0 - sun_sin * sun_sin).max(0.0).sqrt();
    let n = LUT_WIDTH * LUT_HEIGHT;
    // The stratosphere's haze takes the continental's phase whatever the low haze's kind (`STRAT_DEPTH`).
    let continental = Aerosol::named("continental");
    let mut lut = SkyLut {
        aerosol: medium.aerosol,
        strat_aerosol: (medium.aerosol != continental).then_some(continental),
        air: Vec::with_capacity(n),
        haze: Vec::with_capacity(n),
        strat: Vec::with_capacity(n),
        multi: Vec::with_capacity(n),
    };
    for j in 0..LUT_HEIGHT {
        let v = j as f64 / (LUT_HEIGHT - 1) as f64;
        let elevation = std::f64::consts::FRAC_PI_2 * v * v;
        let (view_sin, view_cos) = elevation.sin_cos();
        let steps = view_steps(medium, view_sin);
        for i in 0..LUT_WIDTH {
            let phi = std::f64::consts::PI * i as f64 / (LUT_WIDTH - 1) as f64;
            // The sun sits at azimuth 0 of the table's frame.
            let nu = view_cos * sun_cos * phi.cos() + view_sin * sun_sin;
            let [air, haze, strat, ms] = in_scatter(table, &steps, sun_sin, nu, multi);
            lut.air.push(medium.light_rgb(&air));
            lut.haze.push(medium.light_rgb(&haze));
            lut.strat.push(medium.light_rgb(&strat));
            lut.multi.push(medium.light_rgb(&ms));
        }
    }
    lut
}

/// Mean radiance of the upper hemisphere, and the irradiance it puts on a
/// level surface, both from the table (the disc is not part of the sky).
fn hemisphere_integrals(sky: &dyn Fn(f32, f32) -> Vec3f) -> (Vec3f, Vec3f) {
    const ROWS: usize = 32;
    const COLUMNS: usize = 32;
    let d_el = std::f32::consts::FRAC_PI_2 / ROWS as f32;
    let d_phi = std::f32::consts::PI / COLUMNS as f32;
    let mut radiance_sum = Vec3f::default();
    let mut irradiance = Vec3f::default();
    for j in 0..ROWS {
        let el = (j as f32 + 0.5) * d_el;
        // Solid angle of the cell, doubled for the mirrored half of the sky.
        let d_omega = 2.0 * el.cos() * d_el * d_phi;
        for i in 0..COLUMNS {
            let c = sky((i as f32 + 0.5) * d_phi, el);
            radiance_sum += c * d_omega;
            irradiance += c * (d_omega * el.sin());
        }
    }
    (radiance_sum * (1.0 / (2.0 * std::f32::consts::PI)), irradiance)
}

/// The sun's disc as `Atmosphere::sun_disc` draws it ([`Atmosphere::disc_look`]), for a viewer that draws it itself:
/// its radiance along a direction is [`Self::at`], cut at the horizon.
///
/// The disc is round where the sun truly is and is seen through the air's refraction ([`refraction`]): a direction
/// seen at apparent elevation e comes from e - refraction(e), so the disc is lifted (34' on the horizon) and
/// flattened, its lower limb lifted more than its upper one. Its light as seen stays the key's (`sun_disc_look`). Each
/// part is coloured by the air its own light crosses (`rows`), so at sunset the
/// lower limb is redder and dimmer than the upper, and the limb is darker and redder than the centre (Neckel & Labs
/// 1994, `limb`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SunDiscLook {
    /// The disc's centre as seen, unit length: the sun's direction lifted by the refraction.
    pub dir: Vec3f,
    /// Its angular radius in radians, where the sun truly is: the soft edge is `softness` x radius wide, centred on it.
    pub radius: f32,
    pub softness: f32,
    /// Limb darkening per channel: sum_i limb[i] x mu^i, 1 at the centre, with mu = sqrt(1 - (theta / outer)^2) and
    /// theta the true angle from the centre ([`Self::theta`]).
    pub limb: [Vec3f; DISC_LIMB_TERMS],
    /// The radiance where the limb darkening is 1 at DISC_ROWS apparent elevations evenly from `rows_from` to
    /// `rows_to` (radians, the part of the disc above the horizon), each lit through the air its light crosses
    /// (`refracted_transmittance`); between rows the logarithm is interpolated, beyond them the end row holds.
    pub rows: [Vec3f; DISC_ROWS],
    pub rows_from: f32,
    pub rows_to: f32,
}

impl SunDiscLook {
    /// The disc's outer edge, soft edge included: radius x (1 + softness / 2).
    pub fn outer(&self) -> f32 {
        self.radius * (1.0 + 0.5 * self.softness)
    }

    /// The true angle (radians) between what is seen along unit `d` and the disc's centre: both taken back down
    /// through the refraction ([`unrefracted`]), from the chord, which stays exact for a fraction of a degree.
    pub fn theta(&self, d: Vec3f) -> f32 {
        let chord = (unrefracted(d) - unrefracted(self.dir.normalize())).length();
        2.0 * (0.5 * chord).min(1.0).asin()
    }

    /// The disc's shape at true angle `theta` from its centre, per channel: a smoothstep edge `softness` x radius wide
    /// round the radius, box-filtered over `w` more (a viewer's pixel; 0 for none), times the limb darkening.
    pub fn profile(&self, theta: f32, w: f32) -> Vec3f {
        let outer = self.outer();
        if !(outer > 0.0) || theta >= outer + w {
            return Vec3f::default();
        }
        let inner = self.radius * (1.0 - 0.5 * self.softness);
        let edge = if outer - inner + w > 1.0e-9 { 1.0 - smoothstep(inner - 0.5 * w, outer + 0.5 * w, theta) } else { 1.0 };
        let x = (theta / outer).min(1.0);
        let mu = (1.0 - x * x).max(0.0).sqrt();
        let mut limb = self.limb[DISC_LIMB_TERMS - 1];
        for i in (0..DISC_LIMB_TERMS - 1).rev() {
            limb = limb * mu + self.limb[i];
        }
        vec3f(limb.x.max(0.0), limb.y.max(0.0), limb.z.max(0.0)) * edge
    }

    /// The radiance where the limb darkening is 1 at apparent elevation `e` (radians): `rows`, their logarithm
    /// interpolated.
    pub fn row(&self, e: f32) -> Vec3f {
        let span = self.rows_to - self.rows_from;
        let u = if span > 0.0 { ((e - self.rows_from) / span).clamp(0.0, 1.0) } else { 0.0 };
        let f = u * (DISC_ROWS - 1) as f32;
        let i = (f as usize).min(DISC_ROWS - 2);
        let t = f - i as f32;
        let (a, b) = (self.rows[i], self.rows[i + 1]);
        let mix = |a: f32, b: f32| if a > 0.0 && b > 0.0 { (a.ln() + (b.ln() - a.ln()) * t).exp() } else { a + (b - a) * t };
        vec3f(mix(a.x, b.x), mix(a.y, b.y), mix(a.z, b.z))
    }

    /// The disc's radiance along unit `d`, without the horizon's cut.
    pub fn at(&self, d: Vec3f) -> Vec3f {
        let theta = self.theta(d);
        if !(theta < self.outer()) {
            return Vec3f::default();
        }
        self.row(d.y.clamp(-1.0, 1.0).asin()) * self.profile(theta, 0.0)
    }

    /// The disc's light: its radiance summed over the whole disc where the sun truly is (no horizon, the flattening
    /// left out), each part at the colour it is seen in. Over a sun high in the sky it is the sun's irradiance.
    pub fn light(&self) -> Vec3f {
        self.light_over(false)
    }

    /// The disc's radiance summed over the whole disc (no horizon), each part at the colour it is seen in: where the
    /// sun truly is, or (`seen`) as it is seen, flattened by the refraction (each part's solid angle times
    /// d(seen elevation) / d(true elevation) and the ratio of their cosines).
    fn light_over(&self, seen: bool) -> Vec3f {
        const RINGS: usize = 96;
        const ANGLES: usize = 16;
        let outer = self.outer();
        if !(outer > 0.0) {
            return Vec3f::default();
        }
        let centre = unrefracted(self.dir.normalize());
        let side = if centre.y.abs() < 0.9 { vec3f(0.0, 1.0, 0.0) } else { vec3f(1.0, 0.0, 0.0) };
        let right = Vec3f::cross(centre, side).normalize();
        let up = Vec3f::cross(right, centre);
        let dt = outer / RINGS as f32;
        let dphi = std::f32::consts::TAU / ANGLES as f32;
        let mut sum = Vec3f::default();
        for i in 0..RINGS {
            let theta = (i as f32 + 0.5) * dt;
            let shape = self.profile(theta, 0.0) * (theta.sin() * dt * dphi);
            for j in 0..ANGLES {
                let phi = (j as f32 + 0.5) * dphi;
                let p = centre * theta.cos() + (right * phi.cos() + up * phi.sin()) * theta.sin();
                let e = p.y.clamp(-1.0, 1.0).asin();
                let a = apparent_elevation(e);
                let squeeze = if seen {
                    // a = e + refraction(a): da / de = 1 / (1 - refraction'(a))
                    const H: f32 = 1.0e-4;
                    let slope = (refraction(a + H) - refraction(a - H)) / (2.0 * H);
                    a.cos() / (e.cos().max(1.0e-6) * (1.0 - slope))
                } else {
                    1.0
                };
                sum += self.row(a) * shape * squeeze;
            }
        }
        sum
    }
}

/// The sun's disc for the sun at unit `sun`, `radius` radians wide with a soft edge `softness` x radius wide
/// ([`SunDiscLook`]), its light as it is seen `irradiance` (the key's, `Atmosphere::sun_irradiance`) at every size,
/// softness and elevation: the rows keep their colours relative to one another (the lower limb redder) and are
/// scaled together per channel. Near the horizon the key's straight path toward the true sun crosses more air than
/// the bent path the disc is seen along, so there the scale dims the disc to the key. `horizon` is the elevation the
/// disc is cut at (`Atmosphere::horizon_dip`, 0 or below) and `height` the viewer's height its light is seen from
/// (None: the calibrated VIEWER_HEIGHT), so a sun under level seen from a height has rows lit through its own air.
fn sun_disc_look(medium: &Medium, sun: Vec3f, radius: f32, softness: f32, irradiance: Vec3f, horizon: f32, height: Option<f64>) -> SunDiscLook {
    let through = |e: f64| match height {
        Some(h) => refracted_transmittance_from(medium, e, h),
        None => refracted_transmittance(medium, e),
    };
    let outer = radius * (1.0 + 0.5 * softness);
    let e = sun.y.clamp(-1.0, 1.0).asin();
    let seen = apparent_elevation(e);
    let dir = settled_unit(at_elevation(sun, seen));
    // the rows span the part of the disc above the horizon
    let top = apparent_elevation(e + outer);
    let bottom = apparent_elevation(e - outer).max(horizon).min(top);
    // Each channel's limb darkening: the bands' (Neckel & Labs 1994) weighted by what each band brings the channel
    // in the light of the disc's centre.
    let centre = through(seen.max(bottom).min(top) as f64);
    let lit = centre.iter().any(|t| *t > 0.0);
    let limb: [Vec3f; DISC_LIMB_TERMS] = std::array::from_fn(|i| {
        let c: [f32; 3] = std::array::from_fn(|c| {
            let (mut sum, mut weight) = (0.0f64, 0.0f64);
            for k in 0..BANDS {
                let w = medium.to_rgb[c][k].max(0.0) * if lit { centre[k] } else { 1.0 };
                let um = BAND_NM[k] / 1000.0;
                let [a, b, cc] = LIMB_NL94[i];
                sum += w * (a + b / um + cc / um.powi(5));
                weight += w;
            }
            if weight > 0.0 {
                (sum / weight) as f32
            } else if i == 0 {
                1.0
            } else {
                0.0
            }
        });
        vec3f(c[0], c[1], c[2])
    });
    let mut look = SunDiscLook { dir, radius, softness, limb, rows: [Vec3f::all(1.0); DISC_ROWS], rows_from: bottom, rows_to: top };
    look.rows = std::array::from_fn(|j| {
        let el = bottom + (top - bottom) * j as f32 / (DISC_ROWS - 1) as f32;
        let c = medium.rgb(&through(el as f64));
        // (kept above 0 so the rows' logarithm is finite; a row the ground hides is black to the eye)
        vec3f(c.x.max(1.0e-30), c.y.max(1.0e-30), c.z.max(1.0e-30))
    });
    // The disc's light as it is seen, made the key's.
    let seen = look.light_over(true);
    let scale = |want: f32, got: f32| if got > 0.0 && (want / got).is_finite() { (want / got).max(0.0) } else { 0.0 };
    let scale = vec3f(scale(irradiance.x, seen.x), scale(irradiance.y, seen.y), scale(irradiance.z, seen.z));
    look.rows = look.rows.map(|row| {
        let c = row * scale;
        vec3f(c.x.max(1.0e-30), c.y.max(1.0e-30), c.z.max(1.0e-30))
    });
    look
}

use crate::patterns::smooth as smoothstep;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dir_from_az_el;
    use crate::sky::{luminance, noaa_solar_position, SkyDate};

    fn atmo_at(elevation_deg: f32, azimuth_deg: f32) -> Atmosphere {
        Atmosphere::new(
            dir_from_az_el(azimuth_deg, elevation_deg),
            &AtmosphereParams::default(),
            &SunDiscParams::default(),
        )
    }

    fn blue_over_red(c: Vec3f) -> f32 {
        c.z / c.x.max(1.0e-12)
    }

    fn red_over_blue(c: Vec3f) -> f32 {
        c.x / c.z.max(1.0e-12)
    }

    #[test]
    fn noon_zenith_is_bluer_than_the_horizon() {
        let atmo = atmo_at(60.0, 180.0);
        let zenith = atmo.sky(vec3f(0.0, 1.0, 0.0));
        let horizon = atmo.sky(dir_from_az_el(90.0, 1.0));
        assert!(
            blue_over_red(zenith) > 1.3 * blue_over_red(horizon),
            "zenith {zenith:?} should be bluer than the horizon {horizon:?}"
        );
        // SUN_IRRADIANCE puts a clear high-sun zenith at about half a unit.
        let lum = luminance(zenith);
        assert!(lum > 0.03 && lum < 3.0, "zenith luminance {lum}");
    }

    #[test]
    fn at_sunset_the_horizon_toward_the_sun_is_redder() {
        let atmo = atmo_at(2.0, 270.0);
        let toward = atmo.sky(dir_from_az_el(280.0, 3.0));
        let away = atmo.sky(dir_from_az_el(90.0, 3.0));
        assert!(
            red_over_blue(toward) > 1.5 * red_over_blue(away),
            "toward the sun {toward:?} vs away {away:?}"
        );
    }

    #[test]
    fn the_sky_is_dark_at_astronomical_twilight() {
        let dark = atmo_at(-18.0, 270.0);
        let mut brightest = 0.0f32;
        for el in [0.0f32, 2.0, 5.0, 10.0, 20.0, 45.0, 90.0, -10.0] {
            for az in (0..360).step_by(30) {
                brightest = brightest.max(luminance(dark.sky(dir_from_az_el(az as f32, el))));
            }
        }
        assert!(brightest < 1.0e-3, "brightest direction at -18 deg: {brightest}");
        // Civil twilight still glows: the multiple-scattering term keeps the
        // zenith from going black as soon as the ground is in shadow, at the
        // measured level (Koomen et al. 1952, JOSA 42, 353: 0.06 cd/ft2 at the
        // zenith with the sun 6 deg down, 9.7e-5 in the sky's units of 6690 cd/m2).
        let civil = atmo_at(-6.0, 270.0);
        let zenith = luminance(civil.sky(vec3f(0.0, 1.0, 0.0)));
        let measured = 0.06 * 10.764 / 6690.0;
        assert!(zenith > 0.5 * measured && zenith < 2.0 * measured, "civil twilight zenith {zenith} vs {measured}");
        assert!(zenith > 10.0 * brightest, "-6 deg {zenith} vs -18 deg {brightest}");
    }

    /// Integrates `sun_disc` over a square on the tangent plane at the sun,
    /// with the gnomonic Jacobian dOmega = dx dy / (1 + x^2 + y^2)^(3/2).
    fn disc_energy(atmo: &Atmosphere, half_angle: f32) -> Vec3f {
        let c = atmo.sun_dir();
        let e1 = Vec3f::cross(c, vec3f(0.0, 1.0, 0.0)).normalize();
        let e2 = Vec3f::cross(e1, c).normalize();
        let n = 240;
        let a = half_angle.tan();
        let step = 2.0 * a / n as f32;
        let mut sum = Vec3f::default();
        for j in 0..n {
            for i in 0..n {
                let x = -a + (i as f32 + 0.5) * step;
                let y = -a + (j as f32 + 0.5) * step;
                let d = (c + e1 * x + e2 * y).normalize();
                let jacobian = 1.0 / (1.0 + x * x + y * y).powf(1.5);
                sum += atmo.sun_disc(d) * (jacobian * step * step);
            }
        }
        sum
    }

    #[test]
    fn the_disc_keeps_its_energy_when_it_grows() {
        let sun = dir_from_az_el(200.0, 45.0);
        let make = |size_deg: f32| {
            Atmosphere::new(
                sun,
                &AtmosphereParams::default(),
                &SunDiscParams { size_deg, ..SunDiscParams::default() },
            )
        };
        let small = make(0.53);
        let large = make(5.0);
        let want = luminance(small.sun_irradiance());
        // At the default softness 0.2 the soft edge reaches 1.1 x the radius;
        // integrate out to 1.3 x.
        let e_small = luminance(disc_energy(&small, (0.53f32 * 0.5 * 1.3).to_radians()));
        let e_large = luminance(disc_energy(&large, (5.0f32 * 0.5 * 1.3).to_radians()));
        assert!((e_small / want - 1.0).abs() < 0.03, "0.53 deg disc: {e_small} vs {want}");
        assert!((e_large / want - 1.0).abs() < 0.03, "5 deg disc: {e_large} vs {want}");
        assert!((e_large / e_small - 1.0).abs() < 0.03, "{e_small} vs {e_large}");
        // A larger disc is dimmer per steradian, by about the area ratio (89).
        let ratio = luminance(small.sun_radiance()) / luminance(large.sun_radiance());
        assert!(ratio > 50.0 && ratio < 150.0, "radiance ratio {ratio}");
    }

    #[test]
    fn a_hidden_disc_still_lights_the_scene() {
        let sun = dir_from_az_el(100.0, 30.0);
        let hidden = Atmosphere::new(
            sun,
            &AtmosphereParams::default(),
            &SunDiscParams { visible: false, ..SunDiscParams::default() },
        );
        assert_eq!(hidden.sun_disc(sun), Vec3f::default());
        assert!(luminance(hidden.sun_radiance()) > 0.0);
        let shown = atmo_at(30.0, 100.0);
        let disc = luminance(shown.sun_disc(shown.sun_dir()));
        let sky = luminance(shown.sky(dir_from_az_el(100.0, 60.0)));
        assert!(disc > 1000.0 * sky, "disc {disc} vs sky {sky}");
    }

    #[test]
    fn sun_direction_matches_the_noaa_conversion() {
        let mut sun = SunParams {
            mode: "time".to_string(),
            year: 2024,
            month: 6,
            day: 21,
            hour: 13.7,
            tz_offset: 2.0,
            latitude: 52.37,
            longitude: 4.9,
            ..Default::default()
        };
        let got = sun_direction(&sun);
        // libs/render/tests/one_sun.rs's NOAA-to-engine conversion, reproduced.
        let (el, az) = noaa_solar_position(SkyDate { year: 2024, month: 6, day: 21 }, 13.7, 2.0, 52.37, 4.9);
        let (el, az) = (el.to_radians(), az.to_radians());
        let want = vec3f(el.cos() * az.sin(), el.sin(), -el.cos() * az.cos()).normalize();
        assert!((got - want).length() < 1.0e-5, "{got:?} vs {want:?}");
        // Solar noon in Amsterdam at midsummer (13:42 CEST): 61 deg up, due south (+z).
        let elevation = got.y.asin().to_degrees();
        assert!((elevation - 61.07).abs() < 0.5, "elevation {elevation}");
        assert!(got.z > 0.4 && got.x.abs() < 0.05, "{got:?}");
        // Manual mode is dir_from_az_el.
        sun.mode = "manual".to_string();
        sun.elevation_deg = 12.0;
        sun.azimuth_deg = 250.0;
        assert!((sun_direction(&sun) - dir_from_az_el(250.0, 12.0)).length() < 1.0e-6);
    }

    #[test]
    fn the_sky_is_continuous_across_the_azimuth_wrap() {
        // Sun due north: its vertical plane is also where azimuth wraps 360 -> 0.
        let atmo = atmo_at(20.0, 0.0);
        for el in [1.0f32, 10.0, 40.0] {
            let mut prev = luminance(atmo.sky(dir_from_az_el(-0.5, el)));
            for step in 0..=720 {
                let az = step as f32 * 0.5;
                let now = luminance(atmo.sky(dir_from_az_el(az, el)));
                let jump = (now - prev).abs() / prev.max(1.0e-6);
                assert!(jump < 0.08, "el {el}: jump {jump} at az {az}");
                prev = now;
            }
            // The table stores one half of a mirror-symmetric sky: both halves agree.
            for az in [10.0f32, 60.0, 135.0, 179.5] {
                let a = atmo.sky(dir_from_az_el(az, el));
                let b = atmo.sky(dir_from_az_el(360.0 - az, el));
                assert!((a - b).length() <= 1.0e-4 * a.length(), "el {el} az +-{az}: {a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn the_ground_takes_its_colour_and_meets_the_horizon() {
        let sun = dir_from_az_el(150.0, 35.0);
        let grey = |g: f32| {
            Atmosphere::new(
                sun,
                &AtmosphereParams { ground_color: [g, g, g], ..AtmosphereParams::default() },
                &SunDiscParams::default(),
            )
        };
        let dim = grey(0.1);
        let bright = grey(0.4);
        let down = dir_from_az_el(0.0, -45.0);
        // Four times the albedo is four times the light, and a little more:
        // the brighter ground also lights the air above it, which lights it
        // back (the multiple scattering's ground bounce).
        let ratio = luminance(bright.sky(down)) / luminance(dim.sky(down));
        assert!(ratio >= 4.0 && ratio < 4.0 * 1.05, "ground is linear in its colour, with its bounce: {ratio}");
        let sky_ratio = luminance(bright.sky_irradiance()) / luminance(dim.sky_irradiance());
        assert!(sky_ratio > 1.0 && sky_ratio < 1.2, "the bright ground lights the sky a little: {sky_ratio}");
        // No seam at the horizon: just above and just below agree.
        for az in [0.0f32, 150.0, 300.0] {
            let above = luminance(dim.sky(dir_from_az_el(az, 0.01)));
            let below = luminance(dim.sky(dir_from_az_el(az, -0.01)));
            assert!((above - below).abs() < 0.02 * above, "az {az}: {above} vs {below}");
        }
        // Looking down through the planet transmits nothing; the zenith beats a low ray.
        assert_eq!(dim.transmittance(vec3f(0.0, -1.0, 0.0)), Vec3f::default());
        let up = luminance(dim.transmittance(vec3f(0.0, 1.0, 0.0)));
        let low = luminance(dim.transmittance(dir_from_az_el(0.0, 2.0)));
        assert!(up > low && low > 0.0, "zenith {up} vs 2 deg {low}");
    }

    #[test]
    fn the_sky_stays_finite_for_any_sun() {
        for el in [-90.0f32, -30.0, -6.0, 0.0, 0.3, 10.0, 89.99, 90.0] {
            let atmo = atmo_at(el, 33.0);
            for sky_el in [-60.0f32, -1.0, 0.0, 1.0, 30.0, 90.0] {
                for az in [0.0f32, 33.0, 120.0, 213.0] {
                    let d = dir_from_az_el(az, sky_el);
                    let c = atmo.sky(d) + atmo.sun_disc(d);
                    assert!(c.is_finite() && c.x >= 0.0 && c.y >= 0.0 && c.z >= 0.0, "sun {el}, view ({az}, {sky_el}): {c:?}");
                }
            }
            assert!(atmo.ambient().is_finite() && atmo.sun_radiance().is_finite());
        }
    }

    /// Pins the numbers the module header quotes against the engine's
    /// EnvPreset and HDR-lane scales, relative to SUN_IRRADIANCE where a
    /// retune of the constant should not break them.
    #[test]
    fn the_documented_scale_holds() {
        let noon = atmo_at(90.0, 0.0);
        // Zenith transmittance in luminance: Rayleigh 0.11, haze 0.07 and
        // ozone 0.03 of optical depth in the green.
        let t = luminance(noon.sun_irradiance()) / SUN_IRRADIANCE;
        assert!(t > 0.80 && t < 0.86, "zenith transmittance {t}");
        // A white wall facing the high sun: E / pi, about 5.3 at 20.0.
        let wall = luminance(noon.sun_irradiance()) / std::f32::consts::PI;
        let want = 0.825 * SUN_IRRADIANCE / std::f32::consts::PI;
        assert!((wall / want - 1.0).abs() < 0.05, "noon white wall {wall} vs {want}");
        // The sky's light on level ground against the sun's: a clear noon's
        // 0.10-0.15 (real HDRIs and measured diffuse fractions).
        let share = luminance(noon.sky_irradiance()) / luminance(noon.sun_irradiance());
        assert!(share > 0.10 && share < 0.16, "sky / sun on level ground {share}");
        // A clear high-sun zenith, 30 deg from the sun: about half a unit.
        let high = atmo_at(60.0, 0.0);
        let zenith = luminance(high.sky(vec3f(0.0, 1.0, 0.0))) / SUN_IRRADIANCE;
        assert!(zenith > 0.015 && zenith < 0.04, "zenith / SUN_IRRADIANCE {zenith}");
        // The 0.53 deg disc: about a thousand times EnvPreset::Sunset's 300.
        let disc = luminance(noon.sun_radiance()) / SUN_IRRADIANCE;
        assert!(disc > 5.0e3 && disc < 5.0e4, "disc centre / SUN_IRRADIANCE {disc}");
    }

}

#[cfg(test)]
mod env_tests {
    use super::*;
    use crate::{rotate_y, vec, Env, HdriParams};
    use crate::sky::luminance;

    fn sky_params(elevation_deg: f32, azimuth_deg: f32) -> HdriParams {
        let mut p = HdriParams { mode: "sky".to_string(), ..Default::default() };
        p.sky.sun.mode = "manual".to_string();
        p.sky.sun.elevation_deg = elevation_deg;
        p.sky.sun.azimuth_deg = azimuth_deg;
        p
    }

    #[test]
    fn sky_mode_is_the_atmosphere_and_intensity_scales_it() {
        let p = sky_params(35.0, 120.0);
        let env = Env::new(&p);
        let atmo = Atmosphere::new(dir_from_az_el(120.0, 35.0), &p.sky.atmosphere, &p.sky.sun_disc);
        for &(az, el) in &[(0.0f32, 5.0f32), (120.0, 35.0), (300.0, 70.0), (60.0, -30.0)] {
            let d = dir_from_az_el(az, el);
            let want = atmo.sky(d) + atmo.sun_disc(d);
            let got = env.radiance(d);
            assert!(
                (got - want).length() <= 1.0e-4 * want.length().max(1.0e-3),
                "({az}, {el}): {got:?} vs {want:?}"
            );
        }
        let mut brighter = p.clone();
        brighter.intensity_ev = 1.0;
        let d = dir_from_az_el(0.0, 30.0);
        let ratio = Env::new(&brighter).radiance(d).y / env.radiance(d).y;
        assert!((ratio - 2.0).abs() < 1.0e-4, "one EV up doubles the map: {ratio}");
    }

    #[test]
    fn the_sun_is_the_key_by_day_and_gone_at_night() {
        let sun_dir = dir_from_az_el(200.0, 30.0);
        let day = Env::new(&sky_params(30.0, 200.0));
        let key = day.sun().expect("the sun is up, so it is the key");
        // The key comes from where the sun is seen (the refraction lifts it 1.7' at 30 deg).
        let atmo = Atmosphere::new(sun_dir, &AtmosphereParams::default(), &SunDiscParams::default());
        assert!((key.dir - atmo.disc_dir()).length() < 1.0e-5, "{:?}", key.dir);
        // Radiance over the cone is the sun's irradiance at the ground (1 %:
        // cos_radius is an f32 this close to 1).
        let cone = 2.0 * std::f32::consts::PI * (1.0 - key.cos_radius);
        let ratio = luminance(key.radiance * cone) / luminance(atmo.sun_irradiance());
        assert!((ratio - 1.0).abs() < 0.01, "cone irradiance ratio {ratio}");
        assert!((day.sun_dir().expect("sky mode") - sun_dir).length() < 1.0e-5);

        let night = Env::new(&sky_params(-5.0, 200.0));
        assert!(night.sun().is_none(), "no key once the sun has set");
        assert!(night.sun_dir().expect("sky mode").y < 0.0);

        let studio = HdriParams { mode: "studio".to_string(), ..Default::default() };
        let studio = Env::new(&studio);
        assert!(studio.sun_dir().is_none());
        assert!(studio.sun().is_none(), "no lights, so no key in studio mode");
    }

    /// The sun's key carries its disc's reach: a surface facing a disc under a
    /// degree wide gets all of its emission, and the covering cone is the one
    /// the drawn disc ends at, soft limb included (the cone `radiance` is
    /// averaged over is the nominal disc inside it).
    #[test]
    fn the_suns_key_is_covered_by_its_outer_limb() {
        let (az, el) = (200.0, 30.0);
        let sun_dir = dir_from_az_el(az, el);
        let p = sky_params(el, az);
        let key = Env::new(&p).sun().expect("the sun is up, so it is the key");
        assert!(key.validate().is_ok(), "{key:?}");
        assert_eq!(key.facing, 1.0);
        let atmo = Atmosphere::new(sun_dir, &p.sky.atmosphere, &p.sky.sun_disc);
        assert!((key.cos_cover - atmo.sun_outer_radius().cos()).abs() < 1.0e-7, "{} vs {}", key.cos_cover, atmo.sun_outer_radius().cos());
        assert!(key.cos_cover < key.cos_radius, "a soft limb reaches past the nominal disc");
        // And that really is where the disc ends: lit inside the cone, black outside it.
        let outer_deg = atmo.sun_outer_radius().to_degrees();
        // (off the disc's centre as it is seen, the key's own direction)
        let seen = key.dir.y.asin().to_degrees();
        let inside = dir_from_az_el(az, seen + 0.9 * outer_deg);
        let outside = dir_from_az_el(az, seen + 1.1 * outer_deg);
        assert!(inside.dot(key.dir) > key.cos_cover && luminance(atmo.sun_disc(inside)) > 0.0);
        assert!(outside.dot(key.dir) < key.cos_cover && luminance(atmo.sun_disc(outside)) == 0.0);
        // A hard-edged disc is its own covering cone.
        let mut hard = p.clone();
        hard.sky.sun_disc.softness = 0.0;
        let key = Env::new(&hard).sun().expect("the sun is up");
        assert!((key.cos_cover - key.cos_radius).abs() < 1.0e-7, "{} vs {}", key.cos_cover, key.cos_radius);
        assert!(key.validate().is_ok(), "{key:?}");
    }

    #[test]
    fn rotation_turns_the_sun_and_the_sky_together() {
        let p = sky_params(25.0, 90.0);
        let mut turned = p.clone();
        turned.rotation_deg = 40.0;
        let a = Env::new(&p);
        let b = Env::new(&turned);
        let want = rotate_y(a.sun_dir().unwrap(), 40.0);
        assert!((b.sun_dir().unwrap() - want).length() < 1.0e-5);
        // (the key from where the sun is seen, turned the same way)
        assert!((b.sun().unwrap().dir - rotate_y(a.sun().unwrap().dir, 40.0)).length() < 1.0e-5);
        // The cones and the facing share are about the key's own centre: the yaw leaves them.
        let (ka, kb) = (a.sun().unwrap(), b.sun().unwrap());
        assert_eq!((kb.cos_radius, kb.facing, kb.cos_cover), (ka.cos_radius, ka.facing, ka.cos_cover));
        for &(az, el) in &[(10.0f32, 20.0f32), (200.0, 60.0), (300.0, 5.0)] {
            let d = dir_from_az_el(az, el);
            let ca = a.radiance(d);
            let cb = b.radiance(rotate_y(d, 40.0));
            assert!((ca - cb).length() <= 1.0e-4 * ca.length(), "({az}, {el}): {ca:?} vs {cb:?}");
        }
        // ibl's sign, through the bake: +90 takes a sun at -Z (north) to -X,
        // exactly as EnvMap::procedural turns its presets (ibl.rs's own test).
        // A 10 deg disc makes sure the 64 x 32 texel grid lands on it.
        let mut north = sky_params(30.0, 0.0);
        north.sky.sun_disc.size_deg = 10.0;
        let straight = vec(Env::new(&north).bake(64).brightest_direction());
        assert!(straight.z < -0.75 && straight.y > 0.35 && straight.x.abs() < 0.15, "{straight:?}");
        north.rotation_deg = 90.0;
        let west = vec(Env::new(&north).bake(64).brightest_direction());
        assert!(west.x < -0.75 && west.y > 0.35 && west.z.abs() < 0.15, "{west:?}");
    }

    #[test]
    fn the_bake_is_finite_and_non_negative_at_any_sun() {
        for el in [-90.0f32, -18.0, -6.0, 0.0, 2.0, 45.0, 90.0] {
            let map = Env::new(&sky_params(el, 135.0)).bake(32);
            assert_eq!((map.width, map.height, map.data.len()), (32, 16, 32 * 16));
            assert!(
                map.data.iter().all(|t| {
                    t[0].is_finite() && t[1].is_finite() && t[2].is_finite()
                        && t[0] >= 0.0 && t[1] >= 0.0 && t[2] >= 0.0 && t[3] == 1.0
                }),
                "sun at {el} deg"
            );
        }
    }

    #[test]
    fn the_baked_sun_keeps_its_energy_between_texel_centres() {
        let lum_sum = |map: &makepad_render_material::ibl::EnvMap| -> f64 {
            let mut sum = 0.0f64;
            for y in 0..map.height {
                let theta = (y as f32 + 0.5) / map.height as f32 * std::f32::consts::PI;
                let d_omega = (std::f32::consts::TAU / map.width as f32) * (std::f32::consts::PI / map.height as f32) * theta.sin();
                for x in 0..map.width {
                    let t = map.data[y * map.width + x];
                    sum += (luminance(vec3f(t[0], t[1], t[2])) * d_omega) as f64;
                }
            }
            sum
        };
        // A 4 degree disc in a 64 x 32 map (5.6 degree texels): at texel
        // centres alone it would carry 0 to 2.4 times its energy, depending
        // on where it falls between them. The bake's refinement (A1's
        // refine_hot_spots, fed by Env::hot_spots) averages its texels.
        for az in [100.0f32, 101.4, 102.8, 104.2] {
            let mut p = sky_params(35.0, az);
            p.sky.sun_disc.size_deg = 4.0;
            let with_disc = lum_sum(&Env::new(&p).bake(64));
            p.sky.sun_disc.visible = false;
            let without = lum_sum(&Env::new(&p).bake(64));
            let atmo = Atmosphere::new(dir_from_az_el(az, 35.0), &p.sky.atmosphere, &p.sky.sun_disc);
            let want = luminance(atmo.sun_irradiance()) as f64;
            let got = with_disc - without;
            assert!((got / want - 1.0).abs() < 0.1, "az {az}: the baked disc carries {got} of {want}");
        }
    }

    /// A1's note for this task: `bake_par` refines only when every row was
    /// baked, and until the sun fed `hot_spots` nothing could show it.
    #[test]
    fn a_bake_cut_short_gets_no_refinement() {
        let mut p = sky_params(35.0, 100.0);
        p.sky.sun_disc.size_deg = 4.0;
        let env = Env::new(&p);
        let radiance = |d: Vec3f| env.radiance(d);
        // The texel-centre bake, and the refined one: the sun must matter here.
        let plain = crate::bake_with(64, &radiance);
        let refined = env.bake(64);
        assert_ne!(plain.data, refined.data, "the refinement changes this map");
        // Every row run: the parallel bake refines like the serial one.
        let all = env.bake_par(64, |rows, row| {
            for y in 0..rows {
                row(y);
            }
        });
        assert_eq!(all.data, refined.data);
        // The zenith row skipped (a cancelled job): no refinement, so the
        // rest of the map is the plain bake and the skipped row stays black.
        let cut = env.bake_par(64, |rows, row| {
            for y in 1..rows {
                row(y);
            }
        });
        assert_eq!(&cut.data[64..], &plain.data[64..]);
        assert!(cut.data[..64].iter().all(|t| t[0] == 0.0 && t[1] == 0.0 && t[2] == 0.0 && t[3] == 1.0));
    }
}
