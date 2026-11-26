//! Rain on water (the add-on survey's recommendation 3; the weather lane, 2026-11-09): what falling rain does to the
//! mirror of standing water, the puddles (`Env::rain_puddle`) and the Lake and the skyline's sea (floor.rs).
//!
//! Each drop opens a crater a few millimetres across, which springs back and sends out a ring of capillary-gravity
//! waves. Their dispersion, w^2 = g k + (sigma / rho) k^3, runs the short waves fastest, so a ring is a packet of fine
//! crests running ahead of a longer one at the least group speed (17.8 cm/s, 4.4 cm long), with calm water inside it.
//! The rings are the linear Cauchy-Poisson problem's (Lamb's Hydrodynamics: waves from a disturbance released at
//! rest): the crater (a depression with its rim, of no volume, a few millimetres across) released at rest, its waves
//! read by stationary phase. At radius r and age t the crests there are the ones whose group speed is r / t, their
//! phase k r - w t, their height F(k) sqrt(k / (r t w'')) (F the crater's Hankel transform); at the caustic of the least
//! group speed w'' has its Airy width's worth (`ring`), and inside it the water lies calm. Against the exact Hankel
//! integral this holds each ring's slopes within 10-20 % from 50 ms on (`ring_sp.py` in the lane's run folder). The
//! rings are damped as on natural water, whose surface film leaves it inextensible: k sqrt(nu w / 8) per second, four to
//! seven times clean water's 2 nu k^2 at these lengths.
//!
//! The rain itself: its rate from the rain haze (`rate_mm_h`), Marshall and Palmer's median drop (D0 = 0.89 R^0.21 mm)
//! at its terminal speed (Atlas, Srivastava and Sekhon 1973), as many drops as carry the rate: 1360 a square metre each
//! second at the Rain kind's 5 mm/h. Steady rain lays one ring over another: in a fifth of a second the rings of that
//! rain already cover the water, so only the young ones stand apart (`young`). They are drawn one by one where the
//! texel resolves them (two jittered grids of drops, a 3 x 3 search each, `at`); the older ones and the young ones too
//! fine for the texel are slopes it leaves unresolved, which spread its mirror into a sheen (the Lake's facets,
//! `floor::mirror_lobe`). At 5 mm/h they come to a slope deviation of 0.05 per axis, about the Lake's own wind ripples
//! at Ripples 0.25; the far water's mirror is a sheen and its reflectance drops toward the horizon as the facets turn
//! to the eye.
//!
//! Nothing here is taken from the add-ons: Physical Open Waters' rain ripple credits a CC BY-NC-SA Shadertoy and is
//! not used; this is worked out from the physics above.

use std::sync::OnceLock;

/// Gravity, water's surface tension over its density (m^3 / s^2), its kinematic viscosity (m^2 / s) and density.
const G: f32 = 9.81;
const SIGMA_RHO: f32 = 7.4e-5;
const NU: f32 = 1.0e-6;
const RHO: f32 = 1000.0;
/// The Rain kind's rain (`weather.rs`, 0.6) is a moderate rain of RAIN_KIND_MM_H. The rain haze is a light extinction
/// (`RAIN_ZENITH`), and rain's extinction of light goes with its rate as R^0.67 (ITU-R P.1817: 1.076 R^0.67 dB/km), so
/// the rate goes with (rain / 0.6)^(1 / 0.67): 1.8 mm/h at 0.3, 9.2 at the Storm kind's 0.9, 10.7 at 1.
const RAIN_KIND: f32 = 0.6;
const RAIN_KIND_MM_H: f32 = 5.0;
const RATE_EXPONENT: f32 = 1.0 / 0.67;
/// The crater a drop opens holds about this share of its energy at its deepest (the order Engel 1966 measured);
/// released at rest it carries it into the ring. Its radius is where the hollow's weight and its new surface hold it
/// (a hemisphere's potential energy rho g pi a^4 / 4 and its surface's sigma pi a^2).
const CRATER_SHARE: f32 = 0.25;
/// Rings are drawn from RING_FROM_S: before that the crater and its jet stand where the drop fell.
const RING_FROM_S: f32 = 0.03;
/// The young rings stand apart until the rain's rings cover the water about RING_COVER times over (`RainOnWater::of`).
const RING_COVER: f32 = 1.0;
/// Two jittered grids of drops, each cell one drop at a time.
const LAYERS: usize = 2;
/// A ring's crests are resolved from RING_FULL texel footprints a wavelength, gone at RING_GONE (the Lake's ripples').
const RING_GONE: f32 = 2.5;
const RING_FULL: f32 = 7.0;
/// The young rings are drawn one by one while a texel's footprint holds fewer than FIELD_ONE of them, and fade into
/// their mean slopes by FIELD_ALL.
const FIELD_ONE: f32 = 0.5;
const FIELD_ALL: f32 = 2.0;
/// The table of the crests' wavenumber by group speed (`waves`): its speeds from the least to SPEED_MOST (m/s), their
/// squares' roots evenly spaced (fine by the caustic).
const SPEED_MOST: f32 = 1.2;
const SPEED_ROWS: usize = 192;

/// The dispersion's constants, once: the least group speed and its wavenumber, w''' there, and the crests' wavenumber
/// by group speed on the capillary side.
struct Waves {
    least: f32,
    third: f32,
    k: Vec<f32>,
}

fn omega(k: f32) -> f32 {
    (G * k + SIGMA_RHO * k * k * k).sqrt()
}

fn group(k: f32) -> f32 {
    (G + 3.0 * SIGMA_RHO * k * k) / (2.0 * omega(k))
}

/// w''(k): how fast the group speed grows with the wavenumber, (3 s k - cg^2) / w.
fn second(k: f32) -> f32 {
    let c = group(k);
    (3.0 * SIGMA_RHO * k - c * c) / omega(k)
}

/// The amplitude's damping rate under an inextensible surface film, k sqrt(nu w / 8) per second.
fn damping(k: f32, w: f32) -> f32 {
    k * (NU * w / 8.0).sqrt()
}

fn waves() -> &'static Waves {
    static WAVES: OnceLock<Waves> = OnceLock::new();
    WAVES.get_or_init(|| {
        // the least group speed: 3 s k = cg^2, bisected (the group speed falls to it and rises past it)
        let (mut lo, mut hi) = (10.0f32, 2000.0f32);
        for _ in 0..80 {
            let mid = 0.5 * (lo + hi);
            if 3.0 * SIGMA_RHO * mid - group(mid) * group(mid) < 0.0 {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let kg = 0.5 * (lo + hi);
        let least = group(kg);
        let h = 0.5;
        let third = (second(kg + h) - second(kg - h)) / (2.0 * h);
        let k = (0..SPEED_ROWS)
            .map(|i| {
                let q = i as f32 / (SPEED_ROWS - 1) as f32;
                let v = least + (SPEED_MOST - least) * q * q;
                // the capillary side: the group speed rises with k past kg
                let (mut lo, mut hi) = (kg, 1.0e6f32);
                for _ in 0..80 {
                    let mid = 0.5 * (lo + hi);
                    if group(mid) < v {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                0.5 * (lo + hi)
            })
            .collect();
        Waves { least, third, k }
    })
}

impl Waves {
    /// The crests' wavenumber whose group speed is `v` (at least the least).
    fn k_at(&self, v: f32) -> f32 {
        let q = ((v - self.least).max(0.0) / (SPEED_MOST - self.least)).sqrt() * (SPEED_ROWS - 1) as f32;
        let i = (q.floor() as usize).min(SPEED_ROWS - 2);
        let t = (q - i as f32).min(1.0);
        self.k[i] + (self.k[i + 1] - self.k[i]) * t
    }
}

/// The rain's rate (mm/h) its haze stands for: RAIN_KIND_MM_H at the Rain kind's 0.6, as R^(1 / 0.67) of the haze.
pub(crate) fn rate_mm_h(rain: f32) -> f32 {
    RAIN_KIND_MM_H * (rain.max(0.0) / RAIN_KIND).powf(RATE_EXPONENT)
}

/// Rain falling on standing water at one rate, worked out once a map (`Weather`, `FloorModel::with_rain`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RainOnWater {
    seed: u32,
    /// The crater's radius (m) and its Hankel transform's scale (m^3): F(k) = f0 (k a)^2 exp(-(k a)^2 / 4).
    crater: f32,
    f0: f32,
    /// Rings younger than this (s) are drawn one by one; the grids' cell (m), and how long a cell's drop recurs (s).
    young: f32,
    cell: f32,
    cycle: f32,
    /// The ring's front (m/s): the group speed of the crests where the crater's spectrum has fallen to a hundredth of
    /// its peak; the ring tapers to nothing over its last fifth.
    front: f32,
    /// Young rings a square metre: how many a texel's footprint holds (`at`'s fade into their mean).
    per_m2: f32,
    /// The slopes' variance per axis of the older rings (unresolved everywhere) and the young rings' mean.
    old: f32,
    young_mean: f32,
}

impl RainOnWater {
    /// The rain `rain` (0..1, the weather's) on water, its drops placed by `seed`; None without rain.
    pub(crate) fn of(rain: f32, seed: u32) -> Option<RainOnWater> {
        let rate = rate_mm_h(rain);
        if !(rate > 0.0) {
            return None;
        }
        // Marshall and Palmer's median drop and its terminal speed; as many drops as carry the rate
        let d = 0.89e-3 * rate.powf(0.21);
        let speed = 9.65 - 10.3 * (-0.6 * d * 1.0e3).exp();
        let volume = std::f32::consts::PI * d * d * d / 6.0;
        let drops = rate * 1.0e-3 / 3600.0 / volume;
        let energy = 0.5 * RHO * volume * speed * speed;
        // the crater: rho g pi a^4 / 4 + rho s pi a^2 = CRATER_SHARE x the drop's energy (Newton from 3 mm)
        let held = CRATER_SHARE * energy;
        let mut a = 3.0e-3f32;
        for _ in 0..30 {
            let f = RHO * G * std::f32::consts::PI * a.powi(4) / 4.0 + RHO * SIGMA_RHO * std::f32::consts::PI * a * a - held;
            let df = RHO * G * std::f32::consts::PI * a.powi(3) + 2.0 * RHO * SIGMA_RHO * std::f32::consts::PI * a;
            a = (a - f / df).max(1.0e-4);
        }
        // its released waves carry that energy: pi rho int (g + s k^2) F^2 k dk = held (log steps in k)
        let steps = 600;
        let (k_lo, k_hi) = (5.0f32, 4.0e4f32);
        let step = (k_hi / k_lo).ln() / steps as f32;
        let shape = |k: f32| {
            let ka = k * a;
            ka * ka * (-0.25 * ka * ka).exp()
        };
        let (mut e1, mut s_young, mut s_old) = (0.0f64, 0.0f64, 0.0f64);
        // the young rings stand apart until the rain's rings cover the water RING_COVER times: the front's disc
        // swept over the ages, drops pi v^2 T^3 / 3
        let front = {
            // where the crater's spectrum falls to a hundredth of its peak above it: (k a)^2 e^(-(k a)^2 / 4) = 0.04 / e
            let mut ka = 2.0f32;
            while ka * ka * (-0.25 * ka * ka).exp() > 0.01 * 4.0 / std::f32::consts::E && ka < 20.0 {
                ka += 0.01;
            }
            group(ka / a)
        };
        let young = (3.0 * RING_COVER / (drops * std::f32::consts::PI * front * front)).cbrt().max(2.0 * RING_FROM_S);
        for i in 0..=steps {
            let k = k_lo * (step * i as f32).exp();
            let dk = (k * step) as f64 * if i == 0 || i == steps { 0.5 } else { 1.0 };
            let f2 = (shape(k) * shape(k)) as f64;
            e1 += std::f64::consts::PI * RHO as f64 * (G + SIGMA_RHO * k * k) as f64 * f2 * k as f64 * dk;
            // a ring's slopes summed over the water hold pi int k^3 F^2 dk, falling as exp(-2 gamma t): over the drawn
            // ages and after them
            let rate2 = 2.0 * damping(k, omega(k)) as f64;
            let slopes = std::f64::consts::PI * (k as f64).powi(3) * f2 * dk / rate2;
            s_young += slopes * ((-rate2 * RING_FROM_S as f64).exp() - (-rate2 * young as f64).exp());
            s_old += slopes * (-rate2 * young as f64).exp();
        }
        let f0_2 = held as f64 / e1;
        // per axis: half the slopes' square, times the drops each second
        let per_axis = |s: f64| (0.5 * drops as f64 * f0_2 * s) as f32;
        let per_m2 = drops * (young - RING_FROM_S);
        let cell = (LAYERS as f32 / (drops * young)).sqrt().max(front * young);
        Some(RainOnWater {
            seed,
            crater: a,
            f0: f0_2.sqrt() as f32,
            young,
            cell,
            cycle: LAYERS as f32 / (drops * cell * cell),
            front,
            per_m2,
            old: per_axis(s_old),
            young_mean: per_axis(s_young),
        })
    }

    /// The variance per axis of all the rain's slopes: what water too far for its rings to show leaves unresolved.
    pub(crate) fn all(&self) -> f32 {
        self.old + self.young_mean
    }

    /// The rain's slopes on water at ground point (x, z) (metres from the eye's foot), a texel's footprint `foot` long
    /// along the look and `side` across it (0: an exact lookup, every crest resolved): the young rings' slope there as
    /// far as the footprint resolves them (the height's gradient, x and z), and the variance per axis of the slopes it
    /// leaves unresolved (the young rings' finer crests and the older rings'). Where a footprint holds more than one
    /// young ring they fade into their mean (FIELD_ONE..FIELD_ALL): no gradient, `all`'s variance.
    pub(crate) fn at(&self, x: f32, z: f32, foot: f32, side: f32) -> (f32, f32, f32) {
        let held = foot * side * self.per_m2;
        let drawn = 1.0 - smooth(FIELD_ONE, FIELD_ALL, held);
        if !(drawn > 0.0) {
            return (0.0, 0.0, self.all());
        }
        let r = (x * x + z * z).sqrt();
        let (lx, lz) = if r > 0.0 { (x / r, z / r) } else { (0.0, 1.0) };
        let (mut gx, mut gz, mut left) = (0.0f32, 0.0f32, 0.0f32);
        let w = waves();
        let c = self.cell;
        for layer in 0..LAYERS {
            // each grid offset by its own share of a cell
            let off = 0.5 * layer as f32;
            let (fx, fz) = (x / c + off, z / c + off);
            let (ix, iz) = (fx.floor() as i32, fz.floor() as i32);
            // (the cells' indices wrap, as the crate's lattices' do, noise.rs: an exact lookup past 2^31 cells, some
            // 2e8 m out, saturates its cell, and the neighbour past it wraps to the lattice's far end, whose drop draws
            // nothing here)
            for dj in -1..=1 {
                let j = iz.wrapping_add(dj);
                for di in -1..=1 {
                    let i = ix.wrapping_add(di);
                    let h = super::noise::hash3_u32(i, j, layer as i32, self.seed ^ 0x7a1d_3c55);
                    let age = ((h >> 22) as f32 + 0.5) / 1024.0 * self.cycle;
                    if age < RING_FROM_S || age > self.young {
                        continue;
                    }
                    let (jx, jz) = (((h >> 11) & 0x7ff) as f32 / 2048.0, (h & 0x7ff) as f32 / 2048.0);
                    let (dx, dz) = (x - (i as f32 + jx - off) * c, z - (j as f32 + jz - off) * c);
                    let d2 = dx * dx + dz * dz;
                    let reach = self.front * age;
                    if !(d2 < reach * reach) || !(d2 > 1.0e-12) {
                        continue;
                    }
                    let d = d2.sqrt();
                    let (slope, k) = self.ring(w, d, age);
                    let taper = 1.0 - smooth(0.8 * reach, reach, d);
                    let slope = (slope.0 * taper, slope.1 * taper);
                    // the crests run round the drop: resolved along the radius as the footprint lies there
                    let (ux, uz) = (dx / d, dz / d);
                    let (along, across) = (ux * lx + uz * lz, uz * lx - ux * lz);
                    let f = ((along * foot) * (along * foot) + (across * side) * (across * side)).sqrt();
                    let keep = if f > 0.0 { smooth(RING_GONE, RING_FULL, std::f32::consts::TAU / (k * f)) } else { 1.0 };
                    let turn = k * f;
                    left += 0.25 * slope.1 * slope.1 * ((1.0 - keep * keep) + keep * keep * (turn * turn).min(2.0));
                    if keep > 0.0 {
                        gx += keep * slope.0 * ux;
                        gz += keep * slope.0 * uz;
                    }
                }
            }
        }
        (gx * drawn, gz * drawn, self.old + left * drawn + self.young_mean * (1.0 - drawn))
    }

    /// One ring `d` metres from its drop at `age` seconds: its slope along the radius there and that slope's
    /// amplitude, and the crests' wavenumber.
    fn ring(&self, w: &Waves, d: f32, age: f32) -> ((f32, f32), f32) {
        let k = w.k_at((d / age).max(w.least));
        let om = omega(k);
        let ka = k * self.crater;
        let f = self.f0 * ka * ka * (-0.25 * ka * ka).exp();
        // the caustic of the least group speed: w'' never under its Airy width's worth (0.277 w''' kappa, kappa the
        // caustic's width in k, (2 / (t w'''))^(1/3)), which gives the Airy peak's height there
        let kappa = (2.0 / (age * w.third)).cbrt();
        let second = second(k);
        let curve = (second * second + (0.277 * w.third * kappa) * (0.277 * w.third * kappa)).sqrt();
        let inner = w.least * age;
        let mut amp = f * (k / (d.max(inner) * age * curve)).sqrt() * (-damping(k, om) * age).exp();
        if d < inner {
            // calm inside the caustic: the Airy function's tail
            let x = (inner - d) * kappa;
            amp *= (-(2.0 / 3.0) * x * x.sqrt()).exp();
        }
        let slope = amp * k;
        ((slope * (k * d - om * age).cos(), slope), k)
    }
}

use crate::patterns::smooth;
