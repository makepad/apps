//! Rainbows and fog bows (the rainbow lane, 2026-11-09): the light water drops send back toward the eye from the sun,
//! the part of their phase function that leaves a drop after one internal reflection (the primary bow) and after two
//! (the secondary), worked out per spectral band from the scattering angle and the drops' sizes. A shower's shaft
//! draws the rainbow where the sun lights its rain (cloud_placed.rs `PlacedVolume::rain_light`), the weather's fog slab
//! the fog bow (`Weather::veil`).
//!
//! The physics, per drop of radius a at wavelength lambda (water's index n from Daimon and Masumura 2007, 20 C):
//! - Geometric optics (Descartes, Newton): a ray at impact parameter b = sin i that leaves after p - 1 internal
//!   reflections is turned by D = 2 (i - r) + (p - 1) (pi - 2 r), sin i = n sin r, and carries the share
//!   eps = (1 - R)^2 R^(p - 1) of its power (Fresnel's R at i, averaged over the two polarisations). D has a least
//!   value where cos i = sqrt((n^2 - 1) / (p^2 - 1)): the bow, at 137.5 deg of scattering for p = 2 (42.5 deg from the
//!   antisolar point, red outside) and 129 deg for p = 3 (51 deg, its colours reversed: the rays turn the other way).
//!   The phase function of the rays is p(theta) = 2 b eps |db/dtheta| / sin theta per unit of the drop's extinction
//!   (Q_ext = 2, so the whole phase averages 1 over the sphere), which is infinite at the bow.
//! - Airy theory (Airy 1838; van de Hulst 1957, section 13.24): near the bow the outgoing wavefront is a cubic, and
//!   the light there is 4 pi b eps (k a)^(1/3) c^(-2/3) Ai^2(-z) / sin theta with z = Delta (k a)^(2/3) c^(-1/3),
//!   Delta the angle from the bow toward its lit side and c half of D'' over b at the bow: one peak a little inside
//!   the geometric bow, the supernumerary bands inside it, an exponential fade on its dark side, and the geometric
//!   phase on average away from it. The lit side away from the bow is joined to the geometric phase by the ratio of
//!   the exact geometric phase to the one Airy's quadratic deviation gives (1 at the bow), so the inside of the
//!   primary is as bright as geometric optics makes it (and the band between the bows, Alexander's, gets none).
//!   Lee 1998 (Applied Optics 37, 1506: "Mie theory, Airy theory, and the natural rainbow") compares Airy theory with
//!   Mie theory for natural drop sizes: it holds the bows' colours and positions for rain drops and draws the fog bow's
//!   broad nearly white band and its supernumeraries, a little off in place for drops of a few micrometres.
//! - The drops' sizes: rain by Marshall and Palmer's exponential (N(D) ~ exp(-Lambda D), Lambda = 4.1 R^-0.21 per
//!   mm, rain_water.rs' rate), fog by a gamma distribution of 10 um effective radius and effective variance 0.1
//!   (Hansen and Travis 1974's form; radiation fogs hold drops of a few to 20 um). Each drop counts by its cross
//!   section, so the bow is the phase averaged over D^2 N(D). Over Marshall and Palmer's spread the rain's
//!   supernumeraries fade within the first one or two; the fog's narrower spread keeps them inside its broad bow.
//! - Each band is averaged over its 40 nm (four wavelengths), over the sun's disc (0.53 deg), and, where the map is
//!   read, over the texel (`Bow::phase`): no feature is narrower than the bake can hold, at any width.
//!
//! The bands are taken to Rec.709 through the sun's own light at the ground (`SunBands::light`). Nothing here is taken
//! from the references the owner gave (a CC BY-NC-SA shader and a ring of fixed colours); it is the physics above.

use std::f64::consts::PI;
use std::sync::OnceLock;

use makepad_draw::*;

use super::atmosphere::{SunBands, BANDS, BAND_NM};

/// A bow is lit by the sun's beam through the drops, and seen through them, by exp(-BOW_PASS tau): half of a drop's
/// extinction is diffraction (Q_ext = 2, van de Hulst 1957, section 8.22), which stays within a fraction of a degree of
/// the beam, finer than a bow's colours, so the light that lit a drop and the bow's own light keep it (the fog's bow in
/// weather.rs, the showers' in cloud_placed.rs).
pub(crate) const BOW_PASS: f32 = 0.5;

/// The table of scattering angles: from FROM_DEG to 180 in STEP_DEG steps (bins centred between). Below 90 deg (toward
/// the sun) the bows hold nothing a map shows: the secondary's light there is under 0.04 % of the primary's peak, and it
/// fades to nothing over TAPER_DEG past FROM_DEG.
const FROM_DEG: f64 = 90.0;
const STEP_DEG: f64 = 0.05;
const BINS: usize = 1800;
const TAPER_DEG: f64 = 10.0;
/// The sun's angular radius (its mean, 0.533 deg across): the bow is the drops' phase seen by the disc's light.
const SUN_RADIUS_DEG: f64 = 0.2666;
/// Each 40 nm band is read at four wavelengths across it.
const BAND_SAMPLES: usize = 4;
const BAND_WIDTH_NM: f64 = 40.0;
/// The fog's drops: a gamma distribution of effective radius FOG_RADIUS_M and effective variance 0.1 (Hansen and
/// Travis 1974): n(r) ~ r^7 exp(-r / 1 um), so weighed by its cross section r^9 exp(-r / 1 um), a gamma of shape 10.
const FOG_RADIUS_M: f64 = 10.0e-6;
const FOG_SHAPE: usize = 10;
/// Marshall and Palmer's drops weighed by their cross section, D^2 exp(-Lambda D): a gamma of shape 3 in the radius.
const RAIN_SHAPE: usize = 3;
/// The drop-averaged Airy light H(y) (`SizeMean`) is tabled over y from Y_LOW to Y_HIGH; past Y_FADE it fades into
/// its average, the geometric phase's 1 / (2 pi sqrt(y)), which it holds by Y_HIGH to well under a percent (the
/// drops' spread averages the fringes out long before).
const Y_LOW: f64 = -10.0;
const Y_HIGH: f64 = 10.0;
const Y_FADE: f64 = 8.0;
const Y_STEP: f64 = 0.02;
/// The drops' light outside their bows (`rest_tables`): tabled every REST_STEP_DEG from 0 to 180 degrees of scattering,
/// from REST_RAYS impact parameters a wavelength.
const REST_STEP_DEG: f64 = 0.5;
const REST_BINS: usize = 360;
const REST_RAYS: usize = 100_000;

/// Water's refractive index at `nm` (Daimon and Masumura 2007, Applied Optics 46, 3811: 20 C, four Sellmeier terms).
fn water_index(nm: f64) -> f64 {
    const A: [f64; 4] = [5.684027565e-1, 1.726177391e-1, 2.086189578e-2, 1.130748688e-1];
    const B: [f64; 4] = [5.101829712e-3, 1.821153936e-2, 2.620722293e-2, 1.069792721e1];
    let l2 = (nm / 1000.0) * (nm / 1000.0);
    (1.0 + (0..4).map(|i| A[i] * l2 / (l2 - B[i])).sum::<f64>()).sqrt()
}

/// The share of a ray's power that leaves a drop of index `n` after `p - 1` internal reflections, at incidence `i`:
/// (1 - R)^2 R^(p - 1) for each polarisation (the inside reflections are at the refraction angle, where R is the
/// outside one's), averaged over the two.
fn share(i: f64, n: f64, p: u32) -> f64 {
    let (ci, si) = (i.cos(), i.sin());
    let ct = (1.0 - si * si / (n * n)).max(0.0).sqrt();
    let rs = ((ci - n * ct) / (ci + n * ct)).powi(2);
    let rp = ((ct - n * ci) / (ct + n * ci)).powi(2);
    0.5 * ((1.0 - rs).powi(2) * rs.powi(p as i32 - 1) + (1.0 - rp).powi(2) * rp.powi(p as i32 - 1))
}

/// The deviation of the ray at impact parameter `b` after `p - 1` internal reflections, and its derivative over b.
fn deviation(b: f64, n: f64, p: u32) -> (f64, f64) {
    let (i, r) = (b.asin(), (b / n).asin());
    let d = 2.0 * (i - r) + (p - 1) as f64 * (PI - 2.0 * r);
    let slope = 2.0 / (1.0 - b * b).sqrt() - 2.0 * p as f64 / (n * n - b * b).sqrt();
    (d, slope)
}

/// The scattering angle (0..pi) of a deviation.
fn scattering(d: f64) -> f64 {
    ((d + PI).rem_euclid(2.0 * PI) - PI).abs()
}

/// One bow of one wavelength: its impact parameter, scattering angle, curvature (half of D'' over b), power share,
/// and which way its lit side lies (+1: toward larger scattering angles, the primary's inside; -1: the secondary's
/// outside).
struct Caustic {
    b: f64,
    theta: f64,
    c: f64,
    eps: f64,
    side: f64,
}

impl Caustic {
    fn new(n: f64, p: u32) -> Caustic {
        let pp = (p * p) as f64;
        let b = ((pp - n * n) / (pp - 1.0)).sqrt();
        let (d, _) = deviation(b, n, p);
        let second = 2.0 * b / (1.0 - b * b).powf(1.5) - 2.0 * p as f64 * b / (n * n - b * b).powf(1.5);
        let theta = scattering(d);
        Caustic { b, theta, c: 0.5 * second.abs(), eps: share(b.asin(), n, p), side: if p == 2 { 1.0 } else { -1.0 } }
    }
}

/// Airy's Ai(x): its Maclaurin series up to |x| = 8, the asymptotic forms past it (Abramowitz and Stegun 10.4.59 and
/// 10.4.60, to their second term); at 8 both hold Ai to under 2e-4 of its size.
fn airy(x: f64) -> f64 {
    const C1: f64 = 0.355_028_053_887_817_2;
    const C2: f64 = 0.258_819_403_792_806_8;
    if x > 8.0 {
        let zeta = 2.0 / 3.0 * x * x.sqrt();
        return (-zeta).exp() / (2.0 * PI.sqrt() * x.powf(0.25)) * (1.0 - 5.0 / (72.0 * zeta));
    }
    if x < -8.0 {
        let z = -x;
        let zeta = 2.0 / 3.0 * z * z.sqrt();
        let phase = zeta + 0.25 * PI;
        return (phase.sin() - 5.0 / (72.0 * zeta) * phase.cos()) / (PI.sqrt() * z.powf(0.25));
    }
    let x3 = x * x * x;
    let (mut f, mut g, mut tf, mut tg) = (1.0f64, x, 1.0f64, x);
    for k in 0..80 {
        let k3 = 3.0 * k as f64;
        tf *= x3 / ((k3 + 2.0) * (k3 + 3.0));
        tg *= x3 / ((k3 + 3.0) * (k3 + 4.0));
        f += tf;
        g += tg;
        if tf.abs() + tg.abs() < 1.0e-17 * (f.abs() + g.abs()) {
            break;
        }
    }
    C1 * f - C2 * g
}

/// Airy's light averaged over the drops' sizes: H(y) = E[x^(1/3) Ai^2(-y x^(2/3))] for x the radius over the
/// distribution's scale, drawn from a gamma of the given shape (weighed by the cross section). A drop of radius s x
/// then gives (k s)^(1/3) H(Delta (k s)^(2/3) c^(-1/3)) (`Bow::new`), so one table serves every scale, worked out
/// once a process.
struct SizeMean {
    values: Vec<f64>,
}

impl SizeMean {
    fn of_shape(shape: usize) -> &'static SizeMean {
        static RAIN: OnceLock<SizeMean> = OnceLock::new();
        static FOG: OnceLock<SizeMean> = OnceLock::new();
        let cell = if shape == RAIN_SHAPE { &RAIN } else { &FOG };
        cell.get_or_init(|| SizeMean::new(shape))
    }

    fn new(shape: usize) -> SizeMean {
        // the gamma's density x^(m-1) e^-x on a grid fine enough for the fringes it averages (at y = Y_HIGH a fringe
        // of Ai^2 spans 0.15 of x), out to where it holds nothing
        let m = shape as f64;
        let (dx, top) = (0.02, m + 14.0 * m.sqrt());
        let xs: Vec<(f64, f64, f64)> = (1..(top / dx) as usize)
            .map(|i| {
                let x = i as f64 * dx;
                (x.powf(1.0 / 3.0), x.powf(2.0 / 3.0), ((m - 1.0) * x.ln() - x).exp())
            })
            .collect();
        let total: f64 = xs.iter().map(|&(_, _, w)| w).sum();
        let rows = ((Y_HIGH - Y_LOW) / Y_STEP).round() as usize + 1;
        let values = (0..rows)
            .map(|j| {
                let y = Y_LOW + j as f64 * Y_STEP;
                xs.iter().map(|&(cube, square, w)| w * cube * airy(-y * square).powi(2)).sum::<f64>() / total
            })
            .collect();
        SizeMean { values }
    }

    fn at(&self, y: f64) -> f64 {
        let mean = |y: f64| 1.0 / (2.0 * PI * y.sqrt());
        if y >= Y_HIGH {
            return mean(y);
        }
        let u = ((y - Y_LOW) / Y_STEP).max(0.0);
        let i = (u as usize).min(self.values.len() - 2);
        let t = (u - i as f64).min(1.0);
        let table = self.values[i] + (self.values[i + 1] - self.values[i]) * t;
        if y <= Y_FADE {
            return table;
        }
        let s = (y - Y_FADE) / (Y_HIGH - Y_FADE);
        table + (mean(y) - table) * s * s * (3.0 - 2.0 * s)
    }
}

/// The exact geometric phase over the one Airy's quadratic deviation gives, on a bow's lit side, by the angle from
/// the bow (STEP_DEG apart from 0): 1 at the bow, the shape of the phase away from it. Worked out once a band (it
/// hardly moves with the wavelength), from both of the bow's branches of rays.
fn lit_ratio(n: f64, p: u32, bow: &Caustic) -> Vec<f64> {
    const RAYS: usize = 1500;
    let step = STEP_DEG.to_radians();
    let rows = (180.0 / STEP_DEG) as usize + 1;
    let mut sum = vec![0.0f64; rows];
    // each branch: impact parameters from the bow outward, denser near it (b = bow -/+ span u^2)
    for (from, to) in [(bow.b, 0.0), (bow.b, 1.0)] {
        let rays: Vec<(f64, f64)> = (1..RAYS)
            .map(|k| {
                let u = k as f64 / RAYS as f64;
                let b = (from + (to - from) * u * u).clamp(1.0e-9, 1.0 - 1.0e-12);
                let (d, slope) = deviation(b, n, p);
                let delta = bow.side * (scattering(d) - bow.theta);
                (delta.max(0.0), 2.0 * b * share(b.asin(), n, p) / slope.abs())
            })
            .collect();
        // the branch's angles grow away from the bow: read its phase at each row it spans
        let mut k = 0;
        for (j, row) in sum.iter_mut().enumerate().skip(1) {
            let delta = j as f64 * step;
            while k + 1 < rays.len() && rays[k + 1].0 < delta {
                k += 1;
            }
            if k + 1 >= rays.len() || rays[k].0 > delta {
                continue;
            }
            let (a, b) = (rays[k], rays[k + 1]);
            let t = if b.0 > a.0 { (delta - a.0) / (b.0 - a.0) } else { 0.0 };
            *row += a.1 + (b.1 - a.1) * t;
        }
    }
    let quadratic = 2.0 * bow.b * bow.eps;
    sum.iter().enumerate().map(|(j, s)| if j == 0 { 1.0 } else { s * (bow.c * j as f64 * step).sqrt() / quadratic }).collect()
}

/// The drops a bow is drawn for: their distribution's gamma shape and scale (m).
#[derive(Clone, Copy)]
pub(crate) struct Drops {
    shape: usize,
    scale: f64,
}

impl Drops {
    /// Rain falling at `rate_mm_h`: Marshall and Palmer's Lambda = 4.1 R^-0.21 per mm of diameter.
    pub(crate) fn rain(rate_mm_h: f32) -> Drops {
        let lambda = 4.1 * (rate_mm_h.max(0.01) as f64).powf(-0.21);
        Drops { shape: RAIN_SHAPE, scale: 1.0e-3 / (2.0 * lambda) }
    }

    /// Fog's drops (FOG_RADIUS_M).
    pub(crate) fn fog() -> Drops {
        Drops { shape: FOG_SHAPE, scale: FOG_RADIUS_M / FOG_SHAPE as f64 }
    }
}

/// The drops' light outside their bows and their diffraction, band by band: the light reflected off a drop's outside
/// (p = 0) and the light refracted straight through it (p = 1), by geometric optics with Fresnel's shares (each
/// polarisation's, averaged), which the drops' sizes drop out of; per steradian per unit of the drops' extinction
/// (Q_ext = 2), each band averaged over its BAND_SAMPLES wavelengths, at bins REST_STEP_DEG wide from 0 to 180 degrees.
/// p = 1 carries 44 % of the extinction within 83 degrees of the beam (none past it: a ray grazing the drop is turned
/// most, 2 (90 - r)), p = 0 3.3 % over the whole sphere, 0.0008 a steradian backward. Built once a process.
fn rest_tables() -> &'static [[f64; REST_BINS]; BANDS] {
    static TABLES: OnceLock<Box<[[f64; REST_BINS]; BANDS]>> = OnceLock::new();
    TABLES.get_or_init(|| {
        let mut tables = Box::new([[0.0f64; REST_BINS]; BANDS]);
        let step = REST_STEP_DEG.to_radians();
        for (k, band) in tables.iter_mut().enumerate() {
            for s in 0..BAND_SAMPLES {
                let nm = BAND_NM[k] + BAND_WIDTH_NM * ((s as f64 + 0.5) / BAND_SAMPLES as f64 - 0.5);
                let n = water_index(nm);
                for j in 0..REST_RAYS {
                    // a ray at impact parameter b holds 2 b db of the drop's unit cross section, half of its extinction
                    let b = (j as f64 + 0.5) / REST_RAYS as f64;
                    let (ci, si) = ((1.0 - b * b).sqrt(), b);
                    let ct = (1.0 - si * si / (n * n)).max(0.0).sqrt();
                    let (rs, rp) = (((ci - n * ct) / (ci + n * ct)).powi(2), ((ct - n * ci) / (ct + n * ci)).powi(2));
                    // reflected off the outside, turned pi - 2 i; refracted in and out, turned 2 (i - r)
                    let (i, r) = (b.asin(), (b / n).asin());
                    let rays = [(0.5 * (rs + rp), PI - 2.0 * i), (0.5 * ((1.0 - rs).powi(2) + (1.0 - rp).powi(2)), 2.0 * (i - r))];
                    for (eps, theta) in rays {
                        let bin = ((theta / step) as usize).min(REST_BINS - 1);
                        band[bin] += eps * b / (REST_RAYS * BAND_SAMPLES) as f64;
                    }
                }
            }
            for (j, v) in band.iter_mut().enumerate() {
                *v /= 2.0 * PI * ((j as f64 * step).cos() - ((j + 1) as f64 * step).cos());
            }
        }
        tables
    })
}

/// The share of a drop's extinction its two bows send out (the mean of their phase over the sphere,
/// integral of b (eps_2 + eps_3) db), at 560 nm: about 2.4 %.
pub(crate) fn bows_share() -> f32 {
    let n = water_index(560.0);
    let rays = 2000;
    let sum: f64 = (0..rays)
        .map(|k| {
            let b = (k as f64 + 0.5) / rays as f64;
            b * (share(b.asin(), n, 2) + share(b.asin(), n, 3))
        })
        .sum();
    (sum / rays as f64) as f32
}

/// The bows of one kind of drops under one sun: their phase (per steradian, the drops' extinction's share sent each
/// way) as the sun's light shows it, by the scattering angle, and its running sum for the texel's mean.
pub(crate) struct Bow {
    rgb: Vec<Vec3f>,
    sums: Vec<Vec3f>,
    /// The drops' light outside the bows (`rest_tables`) as the sun's light shows it, at its bins.
    rest: Vec<Vec3f>,
}

impl Bow {
    pub(crate) fn new(drops: Drops, sun: &SunBands) -> Bow {
        let mean = SizeMean::of_shape(drops.shape);
        let step = STEP_DEG.to_radians();
        let angle = |j: usize| (FROM_DEG + (j as f64 + 0.5) * STEP_DEG).to_radians();
        let mut bands = vec![[0.0f64; BINS]; BANDS];
        for (k, band) in bands.iter_mut().enumerate() {
            let centre = water_index(BAND_NM[k]);
            let ratios = [2u32, 3].map(|p| lit_ratio(centre, p, &Caustic::new(centre, p)));
            for i in 0..BAND_SAMPLES {
                let nm = BAND_NM[k] + BAND_WIDTH_NM * ((i as f64 + 0.5) / BAND_SAMPLES as f64 - 0.5);
                let n = water_index(nm);
                let ks = 2.0 * PI / (nm * 1.0e-9) * drops.scale;
                for (p, ratio) in [2u32, 3].into_iter().zip(&ratios) {
                    let bow = Caustic::new(n, p);
                    let amp = 4.0 * PI * bow.b * bow.eps * bow.c.powf(-2.0 / 3.0) * ks.powf(1.0 / 3.0);
                    let y_per = ks.powf(2.0 / 3.0) * bow.c.powf(-1.0 / 3.0);
                    for (j, v) in band.iter_mut().enumerate() {
                        let theta = angle(j);
                        let delta = bow.side * (theta - bow.theta);
                        let lit = if delta > 0.0 {
                            let u = delta / step;
                            let r = (u as usize).min(ratio.len() - 2);
                            ratio[r] + (ratio[r + 1] - ratio[r]) * (u - r as f64).min(1.0)
                        } else {
                            1.0
                        };
                        let taper = if p == 3 { ((theta.to_degrees() - FROM_DEG) / TAPER_DEG).clamp(0.0, 1.0) } else { 1.0 };
                        *v += amp * mean.at(delta * y_per) * lit * taper / theta.sin() / BAND_SAMPLES as f64;
                    }
                }
            }
        }
        // the sun's disc: a ring seen by a disc's light is the ring's profile through the disc's chords (a half
        // circle), mirrored at the antisolar point, where the angle turns back
        let reach = (SUN_RADIUS_DEG / STEP_DEG).ceil() as i64;
        let kernel: Vec<f64> = (-reach..=reach)
            .map(|i| {
                let x = i as f64 * STEP_DEG / SUN_RADIUS_DEG;
                (1.0 - x * x).max(0.0).sqrt()
            })
            .collect();
        let total: f64 = kernel.iter().sum();
        let rgb: Vec<Vec3f> = (0..BINS)
            .map(|j| {
                let s: [f32; BANDS] = std::array::from_fn(|k| {
                    let mut v = 0.0;
                    for (o, w) in kernel.iter().enumerate() {
                        let mut at = j as i64 + o as i64 - reach;
                        if at >= BINS as i64 {
                            at = 2 * BINS as i64 - 1 - at;
                        }
                        v += w * bands[k][at.max(0) as usize];
                    }
                    (v / total / (4.0 * PI)) as f32
                });
                sun.light(&s)
            })
            .collect();
        let mut sums = Vec::with_capacity(BINS + 1);
        let mut run = Vec3f::default();
        sums.push(run);
        for c in &rgb {
            run += *c * step as f32;
            sums.push(run);
        }
        let tables = rest_tables();
        let rest = (0..REST_BINS).map(|j| sun.light(&std::array::from_fn(|k| tables[k][j] as f32))).collect();
        Bow { rgb, sums, rest }
    }

    /// The drops' whole phase outside their diffraction (which stays in the beam, BOW_PASS) at the scattering angle whose
    /// cosine is `cos_theta`, as `phase` gives it: the bows over a texel `texel` (radians) across (`phase`), and the
    /// light reflected off the drops and refracted through them (`rest_tables`, smooth: read at the angle).
    pub(crate) fn scattered(&self, cos_theta: f32, texel: f32) -> Vec3f {
        let u = (cos_theta.clamp(-1.0, 1.0).acos() / REST_STEP_DEG.to_radians() as f32 - 0.5).max(0.0);
        let i = (u as usize).min(REST_BINS - 2);
        let t = (u - i as f32).min(1.0);
        self.phase(cos_theta, texel) + self.rest[i] + (self.rest[i + 1] - self.rest[i]) * t
    }

    /// The bows' phase (per steradian, per unit of the drops' extinction) at the scattering angle whose cosine is
    /// `cos_theta`, as the sun's light at the ground shows it: the mean over a texel `texel` (radians) across, the
    /// table's own where the texel is finer.
    pub(crate) fn phase(&self, cos_theta: f32, texel: f32) -> Vec3f {
        let step = STEP_DEG.to_radians() as f32;
        let from = FROM_DEG.to_radians() as f32;
        let theta = cos_theta.clamp(-1.0, 1.0).acos();
        let half = 0.5 * texel.max(0.0);
        if theta + half <= from {
            return Vec3f::default();
        }
        if half < step {
            let u = ((theta - from) / step - 0.5).max(0.0);
            let i = (u as usize).min(BINS - 2);
            let t = (u - i as f32).min(1.0);
            return self.rgb[i] + (self.rgb[i + 1] - self.rgb[i]) * t;
        }
        // the running sum at an angle: linear inside each bin
        let sum = |a: f32| {
            let u = ((a - from) / step).clamp(0.0, BINS as f32);
            let i = (u as usize).min(BINS - 1);
            self.sums[i] + self.rgb[i] * ((u - i as f32) * step)
        };
        let top = std::f32::consts::PI;
        let (lo, hi) = (theta - half, theta + half);
        // past the antisolar point the angle turns back
        let whole = if hi > top { sum(top) * 2.0 - sum(2.0 * top - hi) - sum(lo) } else { sum(hi) - sum(lo) };
        whole * (1.0 / texel)
    }
}

