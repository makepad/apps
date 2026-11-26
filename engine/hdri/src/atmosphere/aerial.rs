//! What a viewer at a height of its own sees (`AtmosphereParams::viewer_height_m`; item 3 of the sky add-ons survey).
//!
//! Above level the sky is the sky-view table marched from the viewer's height ([`sky_lut`]; from 200 m it is the
//! calibrated table, bit for bit). At and below level three more tables, phase-free as the sky-view table is (the
//! phase is applied for the lookup's own direction, `ViewerSky::shade`):
//!
//! - the band from level down to the ray that grazes the ground, `dip` below it (1.4 deg from 2000 m, 0.45 deg from
//!   200, 0.05 deg from 2): rays that pass over the curved ground and leave through the air, the haze seen over the
//!   horizon from a height;
//! - below the grazing ray, the air between the viewer and the ground the ray meets and that ground's own light
//!   through it: the ground point is lit at its own height and by its own sun height (over a far ground the sun stands
//!   up to `dip` higher or lower than overhead), so a far ground fades into the haze by its distance, as real ground
//!   does, where the older sky blended it over a fixed 3 deg band (aerial perspective as in Hillaire 2020; the lower
//!   half of a sky-view table, which Physical Atmosphere 2 keeps, as an idea; our own code);
//! - the same over any distance ([`ViewerSky::through`]): a floor stands its eye below the viewer and knows how far each
//!   of its texels is, so it is seen through the air over its own distance, and only once. Its ground can lie below the
//!   planet's (a Mountain's valley under a viewer lower than its drop, a flat floor's eye over a lower viewer), so these
//!   rays run on through the planet's ground, the air below it as dense as at the ground.
//!
//! A march puts its steps where the air is densest: a ray that dips falls to its lowest point and rises again, and
//! both stretches crowd toward that point (from a height most of a grazing ray's light comes from round there); a ray
//! that meets the ground crowds toward the ground. Each step is weighed at its middle, as `view_steps` weighs it.

use makepad_draw::*;

use super::{
    apparent_elevation, distance_to_ground, distance_to_mesosphere_top, distance_to_top, gamut_floor, rayleigh_phase,
    refracted_transmittance_from, view_step, Aerosol, DepthTable, Medium, MultiScatter, RayStep, SkyLut, Spectrum, BANDS,
    LUT_HEIGHT, LUT_WIDTH, MESOSPHERE_STEPS, PLANET_RADIUS, SUN_IRRADIANCE, VIEW_STEPS,
};

/// Rows of the band between level and the grazing ray (squared toward the grazing ray, where it changes fastest).
const BAND_ROWS: usize = 16;
/// Steps over the stretch of a dipping ray that falls to its lowest point (the rising stretch takes VIEW_STEPS, as a
/// row of the sky-view table does, so the band's top row is the table's bottom one).
const FALL_STEPS: usize = 16;
/// The table below the grazing ray: rows (squared toward the grazing ray), columns (the azimuth from the sun) and
/// the steps of each ray down to the ground.
const GROUND_ROWS: usize = 48;
const GROUND_COLUMNS: usize = 32;
const GROUND_STEPS: usize = 24;
/// The sun heights the far ground's light is worked out at, over the sun's own height plus or minus `dip`.
const GROUND_SUNS: usize = 5;
/// The dome summed for the far ground's sky light: rows, columns over half of it (the other is its mirror), steps.
const DOME_ROWS: usize = 12;
const DOME_COLUMNS: usize = 24;
const DOME_STEPS: usize = 16;
/// The air over any distance (`ViewerSky::through`): columns, rows (the depression, squared toward level) and the
/// distances, AERIAL_SLICES of them from AERIAL_NEAR_M to AERIAL_FAR_M, each the last times a fixed ratio (1.36).
const AERIAL_COLUMNS: usize = 16;
const AERIAL_ROWS: usize = 32;
const AERIAL_SLICES: usize = 48;
const AERIAL_NEAR_M: f64 = 1.0;
const AERIAL_FAR_M: f64 = 2.0e6;

/// Four phase-free planes of in-scattered light (`in_scatter`'s four parts: the air's, the low haze's, the
/// stratosphere's and the multiple scattering's, each one light's colour in linear Rec.709, `Medium::light_rgb`).
#[derive(Default)]
struct Planes {
    air: Vec<Vec3f>,
    haze: Vec<Vec3f>,
    strat: Vec<Vec3f>,
    multi: Vec<Vec3f>,
}

impl Planes {
    fn with_capacity(n: usize) -> Planes {
        Planes { air: Vec::with_capacity(n), haze: Vec::with_capacity(n), strat: Vec::with_capacity(n), multi: Vec::with_capacity(n) }
    }

    /// Planes of `n` texels, all zero, to be written in any order (`set`).
    fn zeroed(n: usize) -> Planes {
        Planes { air: vec![Vec3f::default(); n], haze: vec![Vec3f::default(); n], strat: vec![Vec3f::default(); n], multi: vec![Vec3f::default(); n] }
    }

    fn push(&mut self, medium: &Medium, [air, haze, strat, multi]: &[Spectrum; 4]) {
        self.air.push(medium.light_rgb(air));
        self.haze.push(medium.light_rgb(haze));
        self.strat.push(medium.light_rgb(strat));
        self.multi.push(medium.light_rgb(multi));
    }

    fn set(&mut self, at: usize, medium: &Medium, [air, haze, strat, multi]: &[Spectrum; 4]) {
        self.air[at] = medium.light_rgb(air);
        self.haze[at] = medium.light_rgb(haze);
        self.strat[at] = medium.light_rgb(strat);
        self.multi[at] = medium.light_rgb(multi);
    }

    /// Every texel of the four planes times `k`.
    fn scale(&mut self, k: f32) {
        for plane in [&mut self.air, &mut self.haze, &mut self.strat, &mut self.multi] {
            plane.iter_mut().for_each(|texel| *texel *= k);
        }
    }

    /// The four planes at texel weights `taps` (index, weight).
    fn read(&self, taps: &[(usize, f32)]) -> [Vec3f; 4] {
        let mut out = [Vec3f::default(); 4];
        for &(i, w) in taps {
            out[0] += self.air[i] * w;
            out[1] += self.haze[i] * w;
            out[2] += self.strat[i] * w;
            out[3] += self.multi[i] * w;
        }
        out
    }
}

/// A ray's steps from radius `r0` at elevation sine `view_sin` between the distances `bounds` (one step between each
/// two, weighed at its middle: `view_step`, the sky-view table's own step), and its optical depth to the last bound.
/// With `depths` it also hands over the optical depth after every step.
fn march(medium: &Medium, view_sin: f64, r0: f64, bounds: &[f64], mut depths: Option<&mut Vec<Spectrum>>) -> (Vec<RayStep>, Spectrum) {
    let mut steps = Vec::with_capacity(bounds.len().saturating_sub(1));
    let mut depth = [0.0f64; BANDS];
    for pair in bounds.windows(2) {
        let (t0, t1) = (pair[0], pair[1]);
        steps.push(view_step(medium, r0, view_sin, 0.5 * (t0 + t1), t1 - t0, &mut depth));
        if let Some(depths) = depths.as_mut() {
            depths.push(depth);
        }
    }
    (steps, depth)
}

/// The bounds of a ray from radius `r0` at elevation sine `view_sin` that leaves through the top of the air, from
/// `from` metres along it: VIEW_STEPS steps over the dense air (crowded toward `from`, as `segment` crowds a row of the
/// sky-view table), then MESOSPHERE_STEPS over the mesosphere above it (as `march_segment` steps it).
fn rising_bounds(r0: f64, view_sin: f64, from: f64, bounds: &mut Vec<f64>) {
    let top = distance_to_top(r0, view_sin);
    crowd_from(from, top, VIEW_STEPS, bounds);
    bounds.pop();
    crowd_from(top, distance_to_mesosphere_top(r0, view_sin).max(top), MESOSPHERE_STEPS, bounds);
}

/// `in_scatter` for a ray from radius `r0`: what reaches the viewer along `steps` with the sun at elevation sine
/// `sun_sin` and view-sun cosine `nu`, per unit of the sun's light above the air, without the phase, in its four parts
/// (the air's, the low haze's, the stratosphere's, the multiple scattering's). With `each` it also hands over the sums
/// after every step (the aerial table's distances).
fn scatter(table: &DepthTable, steps: &[RayStep], r0: f64, sun_sin: f64, nu: f64, multi: &MultiScatter, mut each: Option<&mut dyn FnMut(&[Spectrum; 4])>) -> [Spectrum; 4] {
    let mut sums = [[0.0f64; BANDS]; 4];
    for step in steps {
        let (t, r) = (step.p[0], step.r);
        let mu_sun = (r0 * sun_sin + t * nu) / r;
        let sun_t = table.transmittance_at(&step.depth, mu_sun);
        let psi = multi.at((step.ms_row, step.ms_ty), mu_sun);
        for k in 0..BANDS {
            sums[0][k] += sun_t[k] * step.air[k];
            sums[1][k] += sun_t[k] * step.haze[k];
            sums[2][k] += sun_t[k] * step.strat[k];
            sums[3][k] += (step.air[k] + step.haze[k] + step.strat[k]) * psi[k];
        }
        if let Some(each) = each.as_mut() {
            each(&sums);
        }
    }
    sums
}

/// `n` steps' bounds over [`from`, `to`], quadratically crowded toward `from`: `from + (to - from) u^2`, as `segment`
/// spaces a ray of the sky-view table.
fn crowd_from(from: f64, to: f64, n: usize, bounds: &mut Vec<f64>) {
    for i in 0..=n {
        let u = i as f64 / n as f64;
        bounds.push(from + (to - from) * u * u);
    }
}

/// `n` steps' bounds over [`from`, `to`], crowded toward `to`.
fn crowd_to(from: f64, to: f64, n: usize, bounds: &mut Vec<f64>) {
    for i in 0..=n {
        let u = 1.0 - i as f64 / n as f64;
        bounds.push(to - (to - from) * u * u);
    }
}

/// A transmittance per band as one per channel: the light of a white ground through it (each channel's bands
/// weighted as `Medium::rgb` weighs them).
fn through_rgb(medium: &Medium, t: &Spectrum) -> Vec3f {
    let c = medium.rgb_raw(t) * (1.0 / SUN_IRRADIANCE);
    vec3f(c.x.clamp(0.0, 1.0), c.y.clamp(0.0, 1.0), c.z.clamp(0.0, 1.0))
}

/// The kind whose phase the stratosphere's plane takes: the continental's whatever the low haze's, None when the low
/// haze is continental too (as `build_sky_lut` keeps it).
fn strat_aerosol(medium: &Medium) -> Option<Aerosol> {
    let continental = Aerosol::named("continental");
    (medium.aerosol != continental).then_some(continental)
}

/// The sky-view table from radius `r0` (`build_sky_lut`'s rows and columns, its steps through the dense air and the
/// mesosphere, its four planes).
pub(super) fn sky_lut(medium: &Medium, table: &DepthTable, sun_sin: f64, multi: &MultiScatter, r0: f64) -> SkyLut {
    let sun_cos = (1.0 - sun_sin * sun_sin).max(0.0).sqrt();
    let n = LUT_WIDTH * LUT_HEIGHT;
    let mut lut = SkyLut {
        aerosol: medium.aerosol,
        strat_aerosol: strat_aerosol(medium),
        air: Vec::with_capacity(n),
        haze: Vec::with_capacity(n),
        strat: Vec::with_capacity(n),
        multi: Vec::with_capacity(n),
    };
    let mut bounds = Vec::with_capacity(VIEW_STEPS + MESOSPHERE_STEPS + 1);
    for j in 0..LUT_HEIGHT {
        let v = j as f64 / (LUT_HEIGHT - 1) as f64;
        let elevation = std::f64::consts::FRAC_PI_2 * v * v;
        let (view_sin, view_cos) = elevation.sin_cos();
        bounds.clear();
        rising_bounds(r0, view_sin, 0.0, &mut bounds);
        let (steps, _) = march(medium, view_sin, r0, &bounds, None);
        for i in 0..LUT_WIDTH {
            let phi = std::f64::consts::PI * i as f64 / (LUT_WIDTH - 1) as f64;
            let nu = view_cos * sun_cos * phi.cos() + view_sin * sun_sin;
            let [air, haze, strat, ms] = scatter(table, &steps, r0, sun_sin, nu, multi, None);
            lut.air.push(medium.light_rgb(&air));
            lut.haze.push(medium.light_rgb(&haze));
            lut.strat.push(medium.light_rgb(&strat));
            lut.multi.push(medium.light_rgb(&ms));
        }
    }
    lut
}

/// The light on a level ground at the planet's surface with the sun at elevation `e` (radians), per unit of the sun's
/// light above the air, per band: the sun's from where it is seen, through the bent path (as `Atmosphere::new` lights
/// its ground), and the sky's, summed over a coarse dome marched from the ground.
fn ground_light(medium: &Medium, table: &DepthTable, multi: &MultiScatter, e: f64) -> Spectrum {
    let seen = apparent_elevation(e as f32) as f64;
    let rise = seen.sin().max(0.0);
    let sun = refracted_transmittance_from(medium, seen, 0.0);
    let mut light: Spectrum = std::array::from_fn(|k| sun[k] * rise);
    let (sun_sin, sun_cos) = e.sin_cos();
    let d_el = std::f64::consts::FRAC_PI_2 / DOME_ROWS as f64;
    let d_phi = std::f64::consts::PI / DOME_COLUMNS as f64;
    let strat_kind = strat_aerosol(medium);
    let mut bounds = Vec::with_capacity(DOME_STEPS + MESOSPHERE_STEPS + 1);
    for j in 0..DOME_ROWS {
        let el = (j as f64 + 0.5) * d_el;
        let (s, c) = el.sin_cos();
        bounds.clear();
        // the dense air's DOME_STEPS, then the mesosphere's (which lights a far ground in deep twilight)
        let top = distance_to_top(PLANET_RADIUS, s);
        crowd_from(0.0, top, DOME_STEPS, &mut bounds);
        bounds.pop();
        crowd_from(top, distance_to_mesosphere_top(PLANET_RADIUS, s).max(top), MESOSPHERE_STEPS, &mut bounds);
        let (steps, _) = march(medium, s, PLANET_RADIUS, &bounds, None);
        // a cell's solid angle, doubled for the mirrored half, times its cosine on the level ground
        let weight = 2.0 * c * d_el * d_phi * s;
        for i in 0..DOME_COLUMNS {
            let phi = (i as f64 + 0.5) * d_phi;
            let nu = (c * sun_cos * phi.cos() + s * sun_sin).clamp(-1.0, 1.0);
            let [air, haze, strat, ms] = scatter(table, &steps, PLANET_RADIUS, sun_sin, nu, multi, None);
            let (pr, pm) = (rayleigh_phase(nu), medium.aerosol.phase(nu));
            let ps = strat_kind.as_ref().map_or(pm, |kind| kind.phase(nu));
            for k in 0..BANDS {
                light[k] += (air[k] * pr + haze[k] * pm + strat[k] * ps + ms[k]) * weight;
            }
        }
    }
    light
}

/// The air's own light along a direction below level seen from a viewer height (`Atmosphere::air_light`), in the two
/// parts a cloud over that air relights apart.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AirLight {
    /// The sun's light scattered once toward the viewer by the air, the low haze and the stratosphere's haze, each by
    /// its phase: the Rayleigh blue and the haze's glow toward the sun, which a cloud that hides the sun takes away.
    pub sunlit: Vec3f,
    /// The light scattered toward the viewer after more than one scattering (the isotropic source of Hillaire 2020):
    /// the sky's light and the lit ground's scattered by the air on the way, which a deck replaces with its own.
    pub diffuse: Vec3f,
}

impl AirLight {
    /// The air's light as the sky draws it: both parts, held inside the gamut.
    pub fn total(&self) -> Vec3f {
        gamut_floor(self.sunlit + self.diffuse)
    }

    /// The air's light under clouds that relight its parts apart (`weather::AirRelight`): the sunlit part times
    /// `sunlit`, the diffuse part times `diffuse`, per channel, held inside the gamut as [`Self::total`] is. The parts
    /// are raw (`Medium::rgb_raw`), and a low sun's single scattering has a negative blue that only the floor of their
    /// sum holds: from 2000 m with the sun 1 degree up, 1 degree down toward it, the parts' blue sums to -0.10 where
    /// the red is 12.9. Factors of 1 give `total` bit for bit.
    pub fn relit(&self, sunlit: Vec3f, diffuse: Vec3f) -> Vec3f {
        gamut_floor(self.sunlit * sunlit + self.diffuse * diffuse)
    }
}

/// What a viewer at its own height sees at and below level (the module's header).
pub(super) struct ViewerSky {
    /// The haze's kind, whose phase a lookup applies, and the stratosphere's when it is another (`strat_aerosol`).
    aerosol: Aerosol,
    strat_aerosol: Option<Aerosol>,
    /// How far below level the ray that grazes the ground lies, radians.
    dip: f32,
    /// The ground's albedo (`ground_color`).
    albedo: Vec3f,
    /// The band from the grazing ray (row 0) up to level (the last row): BAND_ROWS rows of LUT_WIDTH.
    band: Planes,
    /// Below the grazing ray (row 0) down to the nadir: GROUND_ROWS rows of GROUND_COLUMNS, the air's light
    /// (`ground`), the ground's own light through it per unit of albedo (`lit`), and the air's transmittance to the
    /// ground, one per row (`ground_through`).
    ground: Planes,
    lit: Vec<Vec3f>,
    ground_through: Vec<Vec3f>,
    /// The air over each of AERIAL_SLICES distances, below level: AERIAL_ROWS rows (row 0 level, the last the nadir)
    /// of AERIAL_SLICES slices of AERIAL_COLUMNS, and its transmittance, a row of slices each. Slice k ends
    /// AERIAL_NEAR_M x `slice_ratio`^k away (`slice_ln` its logarithm).
    aerial: Planes,
    aerial_through: Vec<Vec3f>,
    slice_ratio: f64,
    slice_ln: f64,
}

impl ViewerSky {
    /// The viewer at radius `r0` (above the calibrated 200 m or below it), the sun at elevation sine `sun_sin`, the
    /// ground of albedo `albedo` below.
    pub(super) fn new(medium: &Medium, table: &DepthTable, multi: &MultiScatter, sun_sin: f64, r0: f64, albedo: Vec3f) -> ViewerSky {
        let sun_cos = (1.0 - sun_sin * sun_sin).max(0.0).sqrt();
        let dip = (PLANET_RADIUS / r0).min(1.0).acos();
        let mut bounds = Vec::with_capacity(FALL_STEPS + VIEW_STEPS + MESOSPHERE_STEPS + 2);

        // The band: a ray at e falls to its lowest point, -r0 sin e along it, then rises; at level it only rises, its
        // falling stretch has no length and the row is the sky-view table's bottom one.
        let mut band = Planes::with_capacity(BAND_ROWS * LUT_WIDTH);
        for j in 0..BAND_ROWS {
            let v = j as f64 / (BAND_ROWS - 1) as f64;
            let e = -dip + dip * v * v;
            let (view_sin, view_cos) = e.sin_cos();
            let lowest = (-r0 * view_sin).max(0.0);
            bounds.clear();
            crowd_to(0.0, lowest, FALL_STEPS, &mut bounds);
            bounds.pop();
            rising_bounds(r0, view_sin, lowest, &mut bounds);
            let (steps, _) = march(medium, view_sin, r0, &bounds, None);
            for i in 0..LUT_WIDTH {
                let phi = std::f64::consts::PI * i as f64 / (LUT_WIDTH - 1) as f64;
                let nu = view_cos * sun_cos * phi.cos() + view_sin * sun_sin;
                band.push(medium, &scatter(table, &steps, r0, sun_sin, nu, multi, None));
            }
        }

        // The far ground's light at sun heights over the sun's own plus or minus `dip`: a ground point an arc a from
        // the viewer's foot sees the sun up to a higher or lower.
        let sun_el = sun_sin.clamp(-1.0, 1.0).asin();
        let suns: Vec<(f64, Spectrum)> = (0..GROUND_SUNS)
            .map(|s| {
                let e = sun_el + dip * (2.0 * s as f64 / (GROUND_SUNS - 1) as f64 - 1.0);
                (e, ground_light(medium, table, multi, e))
            })
            .collect();
        let light_at = |e: f64| -> Spectrum {
            let x = if dip > 0.0 { ((e - suns[0].0) / (2.0 * dip) * (GROUND_SUNS - 1) as f64).clamp(0.0, (GROUND_SUNS - 1) as f64) } else { 0.0 };
            let i = (x as usize).min(GROUND_SUNS - 2);
            let t = x - i as f64;
            std::array::from_fn(|k| suns[i].1[k] + (suns[i + 1].1[k] - suns[i].1[k]) * t)
        };

        // Below the grazing ray: each ray down to the ground, its steps crowded toward it.
        let mut ground = Planes::with_capacity(GROUND_ROWS * GROUND_COLUMNS);
        let mut lit = Vec::with_capacity(GROUND_ROWS * GROUND_COLUMNS);
        let mut ground_through = Vec::with_capacity(GROUND_ROWS);
        for j in 0..GROUND_ROWS {
            let v = j as f64 / (GROUND_ROWS - 1) as f64;
            let e = -dip - (std::f64::consts::FRAC_PI_2 - dip) * v * v;
            let (view_sin, view_cos) = e.sin_cos();
            let length = distance_to_ground(r0, view_sin);
            bounds.clear();
            crowd_to(0.0, length, GROUND_STEPS, &mut bounds);
            let (steps, depth) = march(medium, view_sin, r0, &bounds, None);
            let through: Spectrum = depth.map(|d| (-d).exp());
            ground_through.push(through_rgb(medium, &through));
            for i in 0..GROUND_COLUMNS {
                let phi = std::f64::consts::PI * i as f64 / (GROUND_COLUMNS - 1) as f64;
                let nu = view_cos * sun_cos * phi.cos() + view_sin * sun_sin;
                ground.push(medium, &scatter(table, &steps, r0, sun_sin, nu, multi, None));
                // the ground point's own sun height: the sun's cosine to the vertical there
                let mu_sun = ((r0 * sun_sin + length * nu) / PLANET_RADIUS).clamp(-1.0, 1.0);
                let light = light_at(mu_sun.asin());
                let seen: Spectrum = std::array::from_fn(|k| through[k] * light[k] / std::f64::consts::PI);
                lit.push(medium.light_rgb(&seen));
            }
        }

        // The air over any distance below level: a ray's steps are its slices (each the last times a fixed ratio), cut
        // where the ray leaves the air; past that a slice has no length and holds the sums. A ray that meets the
        // planet's ground runs on through it (`densities` holds the air below at the ground's), for a floor's ground
        // that lies lower: cut at the planet's ground, a valley 23 km off seen from 200 m took 6 km of air. The
        // slices end 2000 km out, so a ray toward the nadir stays thousands of kilometres from the planet's centre.
        let ratio = (AERIAL_FAR_M / AERIAL_NEAR_M).powf(1.0 / (AERIAL_SLICES - 1) as f64);
        let mut aerial = Planes::zeroed(AERIAL_ROWS * AERIAL_SLICES * AERIAL_COLUMNS);
        let mut aerial_through = Vec::with_capacity(AERIAL_ROWS * AERIAL_SLICES);
        let mut depths = Vec::with_capacity(AERIAL_SLICES);
        for j in 0..AERIAL_ROWS {
            let v = j as f64 / (AERIAL_ROWS - 1) as f64;
            let e = -std::f64::consts::FRAC_PI_2 * v * v;
            let (view_sin, view_cos) = e.sin_cos();
            let end = distance_to_top(r0, view_sin);
            bounds.clear();
            bounds.push(0.0);
            bounds.extend((0..AERIAL_SLICES).map(|k| (AERIAL_NEAR_M * (ratio.ln() * k as f64).exp()).min(end)));
            depths.clear();
            let (steps, _) = march(medium, view_sin, r0, &bounds, Some(&mut depths));
            // the transmittance after each slice, from the march's own optical depth
            for depth in &depths {
                aerial_through.push(through_rgb(medium, &depth.map(|d| (-d).exp())));
            }
            for i in 0..AERIAL_COLUMNS {
                let phi = std::f64::consts::PI * i as f64 / (AERIAL_COLUMNS - 1) as f64;
                let nu = view_cos * sun_cos * phi.cos() + view_sin * sun_sin;
                let mut k = 0;
                let mut record = |sums: &[Spectrum; 4]| {
                    aerial.set((j * AERIAL_SLICES + k) * AERIAL_COLUMNS + i, medium, sums);
                    k += 1;
                };
                scatter(table, &steps, r0, sun_sin, nu, multi, Some(&mut record));
            }
        }

        ViewerSky { aerosol: medium.aerosol, strat_aerosol: strat_aerosol(medium), dip: dip as f32, albedo, band, ground, lit, ground_through, aerial, aerial_through, slice_ratio: ratio, slice_ln: ratio.ln() }
    }

    /// The angle below level of the ray that grazes the ground, radians.
    pub(super) fn dip(&self) -> f32 {
        self.dip
    }

    /// Every light it gives times `k` (`Atmosphere::lifted`): the band's haze, the air's light and the ground's own
    /// light through it. The transmittances stay, so a caller's ground is seen through the air as it was.
    pub(super) fn scale(&mut self, k: f32) {
        self.band.scale(k);
        self.ground.scale(k);
        self.aerial.scale(k);
        self.lit.iter_mut().for_each(|texel| *texel *= k);
    }

    /// The in-scattered light `planes` at view elevation `e` and azimuth `phi` from the sun: the phases for that
    /// direction applied, then the gamut floor.
    fn shade(&self, planes: [Vec3f; 4], phi: f32, e: f32, sun_sin: f32, sun_cos: f32) -> Vec3f {
        self.shade_parts(planes, phi, e, sun_sin, sun_cos).total()
    }

    /// [`Self::shade`] in its two parts ([`AirLight`]): the single scattering with each part's phase, and the multiple
    /// scattering.
    fn shade_parts(&self, [air, haze, strat, multi]: [Vec3f; 4], phi: f32, e: f32, sun_sin: f32, sun_cos: f32) -> AirLight {
        let nu = (e.cos() * sun_cos * phi.cos() + e.sin() * sun_sin).clamp(-1.0, 1.0) as f64;
        let haze_phase = self.aerosol.phase(nu) as f32;
        let strat_phase = self.strat_aerosol.as_ref().map_or(haze_phase, |kind| kind.phase(nu) as f32);
        AirLight { sunlit: air * rayleigh_phase(nu) as f32 + haze * haze_phase + strat * strat_phase, diffuse: multi }
    }

    /// The four taps of a bilinear read at (`fx`, `fy`) of a plane `w` texels wide and `h` high.
    fn taps(fx: f32, fy: f32, w: usize, h: usize) -> [(usize, f32); 4] {
        let x0 = (fx.max(0.0) as usize).min(w - 2);
        let y0 = (fy.max(0.0) as usize).min(h - 2);
        let (tx, ty) = ((fx - x0 as f32).clamp(0.0, 1.0), (fy - y0 as f32).clamp(0.0, 1.0));
        let (r0, r1) = (y0 * w, (y0 + 1) * w);
        [(r0 + x0, (1.0 - tx) * (1.0 - ty)), (r0 + x0 + 1, tx * (1.0 - ty)), (r1 + x0, (1.0 - tx) * ty), (r1 + x0 + 1, tx * ty)]
    }

    /// The column of azimuth `phi` (0..pi from the sun) in a table `w` columns wide.
    fn column(phi: f32, w: usize) -> f32 {
        (phi / std::f32::consts::PI).clamp(0.0, 1.0) * (w - 1) as f32
    }

    /// Below the grazing ray: the row of elevation `e`, and the air's light there without the ground's.
    fn ground_row(&self, e: f32) -> f32 {
        let span = std::f32::consts::FRAC_PI_2 - self.dip;
        ((-self.dip - e) / span).clamp(0.0, 1.0).sqrt() * (GROUND_ROWS - 1) as f32
    }

    /// The band's haze at elevation `e` between the grazing ray and level, azimuth `phi` from the sun.
    fn band_at(&self, phi: f32, e: f32, sun_sin: f32, sun_cos: f32) -> Vec3f {
        self.band_parts(phi, e, sun_sin, sun_cos).total()
    }

    /// [`Self::band_at`] in its two parts.
    fn band_parts(&self, phi: f32, e: f32, sun_sin: f32, sun_cos: f32) -> AirLight {
        let fy = if self.dip > 0.0 { ((e + self.dip) / self.dip).clamp(0.0, 1.0).sqrt() * (BAND_ROWS - 1) as f32 } else { (BAND_ROWS - 1) as f32 };
        let taps = Self::taps(Self::column(phi, LUT_WIDTH), fy, LUT_WIDTH, BAND_ROWS);
        self.shade_parts(self.band.read(&taps), phi, e, sun_sin, sun_cos)
    }

    /// The sky along a direction below level (elevation `e`, azimuth `phi` from the sun): over the grazing ray the
    /// band's haze, below it the ground of albedo `albedo` through the air. `albedo` None is the ground's own.
    pub(super) fn below(&self, phi: f32, e: f32, sun_sin: f32, sun_cos: f32, albedo: Option<Vec3f>) -> Vec3f {
        if e >= -self.dip {
            return self.band_at(phi, e, sun_sin, sun_cos);
        }
        let taps = Self::taps(Self::column(phi, GROUND_COLUMNS), self.ground_row(e), GROUND_COLUMNS, GROUND_ROWS);
        let lit: Vec3f = taps.iter().map(|&(i, w)| self.lit[i] * w).fold(Vec3f::default(), |a, b| a + b);
        self.shade(self.ground.read(&taps), phi, e, sun_sin, sun_cos) + lit * albedo.unwrap_or(self.albedo)
    }

    /// [`Self::below`]'s air alone, without the ground's light through it (over the grazing ray, where the ray meets no
    /// ground, all of `below`), in its two parts.
    pub(super) fn air_below(&self, phi: f32, e: f32, sun_sin: f32, sun_cos: f32) -> AirLight {
        if e >= -self.dip {
            return self.band_parts(phi, e, sun_sin, sun_cos);
        }
        let taps = Self::taps(Self::column(phi, GROUND_COLUMNS), self.ground_row(e), GROUND_COLUMNS, GROUND_ROWS);
        self.shade_parts(self.ground.read(&taps), phi, e, sun_sin, sun_cos)
    }

    /// [`Self::below`] over a ground of radiance `ground` where the ray meets the planet's ground (the caller's own
    /// ground, lit as it lit it), seen through the air to there.
    pub(super) fn over(&self, phi: f32, e: f32, sun_sin: f32, sun_cos: f32, ground: Vec3f) -> Vec3f {
        if e >= -self.dip {
            return self.below(phi, e, sun_sin, sun_cos, None);
        }
        let fy = self.ground_row(e);
        let taps = Self::taps(Self::column(phi, GROUND_COLUMNS), fy, GROUND_COLUMNS, GROUND_ROWS);
        let y0 = (fy as usize).min(GROUND_ROWS - 2);
        let through = self.ground_through[y0].mix(self.ground_through[y0 + 1], (fy - y0 as f32).clamp(0.0, 1.0));
        self.shade(self.ground.read(&taps), phi, e, sun_sin, sun_cos) + ground * through
    }

    /// A ground of radiance `ground` `dist` metres away along a direction below level (elevation `e`, azimuth `phi`
    /// from the sun), seen through the air between: what the air scatters toward the viewer on the way plus the ground
    /// times the air's transmittance. Short of a metre the air's share falls to none in proportion; past the top of the
    /// air the top's. A ground below the planet's is seen through the air on below it.
    pub(super) fn through(&self, phi: f32, e: f32, dist: f32, sun_sin: f32, sun_cos: f32, ground: Vec3f) -> Vec3f {
        let (taps, through) = self.aerial_taps(phi, e, dist);
        self.shade(self.aerial.read(&taps), phi, e, sun_sin, sun_cos) + ground * through
    }

    /// [`Self::through`]'s air alone, without the ground's light through it, in its two parts.
    pub(super) fn air_through(&self, phi: f32, e: f32, dist: f32, sun_sin: f32, sun_cos: f32) -> AirLight {
        let (taps, _) = self.aerial_taps(phi, e, dist);
        self.shade_parts(self.aerial.read(&taps), phi, e, sun_sin, sun_cos)
    }

    /// The aerial table's eight taps (two rows, two slices, two columns) at elevation `e`, azimuth `phi` and distance
    /// `dist`, and the air's transmittance over that distance.
    fn aerial_taps(&self, phi: f32, e: f32, dist: f32) -> ([(usize, f32); 8], Vec3f) {
        let fy = (-e / std::f32::consts::FRAC_PI_2).clamp(0.0, 1.0).sqrt() * (AERIAL_ROWS - 1) as f32;
        let fx = Self::column(phi, AERIAL_COLUMNS);
        let (x0, y0) = ((fx as usize).min(AERIAL_COLUMNS - 2), (fy as usize).min(AERIAL_ROWS - 2));
        let (tx, ty) = ((fx - x0 as f32).clamp(0.0, 1.0), (fy - y0 as f32).clamp(0.0, 1.0));
        // the slices round the distance (k1 the one at or past it), and the weight of k1, linear in the distance; k0
        // None is no distance at all
        let dist = if dist.is_finite() { dist.max(0.0) as f64 } else { AERIAL_FAR_M };
        let (k0, k1, tk) = if dist <= AERIAL_NEAR_M {
            (None, 0, (dist / AERIAL_NEAR_M) as f32)
        } else {
            let k0 = (((dist / AERIAL_NEAR_M).ln() / self.slice_ln) as usize).min(AERIAL_SLICES - 2);
            let d0 = AERIAL_NEAR_M * (self.slice_ln * k0 as f64).exp();
            let d1 = d0 * self.slice_ratio;
            (Some(k0), k0 + 1, ((dist - d0) / (d1 - d0)).clamp(0.0, 1.0) as f32)
        };
        let mut taps = [(0usize, 0.0f32); 8];
        let mut through = Vec3f::default();
        for (r, (row, wy)) in [(y0, 1.0 - ty), (y0 + 1, ty)].into_iter().enumerate() {
            for (s, (slice, wk)) in [(k0, 1.0 - tk), (Some(k1), tk)].into_iter().enumerate() {
                let w = wy * wk;
                match slice {
                    Some(k) => {
                        through += self.aerial_through[row * AERIAL_SLICES + k] * w;
                        let at = (row * AERIAL_SLICES + k) * AERIAL_COLUMNS + x0;
                        taps[4 * r + 2 * s] = (at, w * (1.0 - tx));
                        taps[4 * r + 2 * s + 1] = (at + 1, w * tx);
                    }
                    // no distance: no air, all of the ground
                    None => through += Vec3f::all(w),
                }
            }
        }
        (taps, through)
    }
}
