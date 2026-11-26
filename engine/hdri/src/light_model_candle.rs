//! Candle light (`ModifierKind::Candle`, `CandleParams`): candles, their flames, what holds them and what they stand
//! on, made in code from the settings and cast per ray as the other lights' models are (light_model.rs), in metres.
//!
//! The model's frame: its origin the mount's point (`SpaceParams::position_m`), y straight up, z level toward the
//! camera turned by the face's yaw, x to the viewer's right. Everything solid is a triangle list under a `TriBvh`,
//! made of surfaces of revolution (the wax, a tea light's cup, a votive's glass, a turned candlestick, a pedestal) and
//! swept tubes (a candelabra's or chandelier's arms, a sconce's arm, wicks, a chain). The flames are not triangles:
//! each is a teardrop of glowing gas, an emission field integrated along the ray (`Flame::along`), so it reads the
//! same from every side, round at its base and pointed at its tip, its middle whiter, its edge and tip deeper orange,
//! a thin blue skirt at its foot and the wick dark in its inner cone.
//!
//! What a ray meets is shaded where it meets it, in three parts the studio adds up (`CandleHit`):
//! - `rel`: light in the light's own colour (times `LightParams`' colour): the flames, and what their light does on
//!   every surface: Lambertian on wax, wood, stone and metal under each flame's intensity with its shadow cast
//!   through the model (glass passes it, tinted), the flames mirrored in glossy faces (a lacquered table, a brass
//!   cup, the glass) blurred by the face's roughness, and the glow the wax passes on below its top (a pillar's wall
//!   round its well lights up, a taper's tip);
//! - `plain`: light at the flame's luminance but its own colour (the blue skirt, a wick's ember);
//! - `abs`: the scene's own light on the model (`CandleLight`: the sky and the sun outdoors, the room's light in a
//!   room), set once the scene is measured.
//!
//! Glass is see-through: a ray crosses each of its faces (both walls of a votive), losing what their Fresnel
//! reflects and their tint takes, and keeps going to the candle and flame inside and out again. What it lets through
//! of the map behind is the `1 - opacity` the studio blends over.
//!
//! Outdoors (no room) a candle standing on the floor also lights the floor round it (`pool`), added over whatever the
//! map draws there; in a room the room is lit by the flames itself (studio.rs: each flame is a light that shines all
//! round).
//!
//! Sizes: a tea light 38 mm across in its aluminium cup, its flame 24 mm tall; a taper 22 mm, flame 32 mm; a pillar
//! 75 mm, flame 36 mm (`CandleParams::flame_size`). A candle flame is about 1 cd; its mean luminance is the light's
//! (intensity EV over the scene's unit, 6690 cd/m2), so its intensity follows from its drawn area.

use makepad_draw::*;

use crate::floor::FloorLighting;
use crate::tri_bvh::TriBvh;
use crate::noise::hash01;
use crate::params::{CandleHolder, CandleKind, CandleLayout, CandleMetal, CandleMount, CandleParams};
use crate::studio::kelvin_to_rgb;

/// Sides round a surface of revolution, a tube and a wick.
const LATHE_SIDES: usize = 36;
/// The longest a lathe's profile segment is drawn in one piece, metres.
const LATHE_STEP: f32 = 0.025;
const TUBE_SIDES: usize = 10;
const WICK_SIDES: usize = 6;
/// Steps along a flame's chord through its emission (midpoint rule).
const FLAME_STEPS: usize = 12;
/// How far a ray steps past a glass face before it is cast again, metres.
const PAST: f32 = 2.0e-5;
/// Glass faces a ray crosses at most (a votive's four, twice over for two votives in a line, and a little more).
const MAX_CROSSINGS: usize = 10;
/// A flame's colour (Kelvin) at its whitest middle and its deepest edge and tip; the light's own colour is the mean
/// (`CANDLE_KELVIN`), so these are ratios to it.
const CORE_KELVIN: f32 = 2350.0;
const EDGE_KELVIN: f32 = 1600.0;
/// The faint glow of hot air and soot round a flame, as a share of its mean radiance and its reach as a share of its
/// width (a little bloom per light, never a lens effect).
const HALO: f32 = 0.025;
const HALO_REACH: f32 = 0.7;
/// The floor's albedo a candle on it lights outdoors (the ground's own is not known here).
const POOL_ALBEDO: f32 = 0.3;
/// How far from a floor candle its pool reaches, metres.
const POOL_REACH: f32 = 1.6;
/// Wax passes light on below its top: a share of the light at its top reaching depth d is e^(-d / WAX_REACH).
const WAX_REACH: f32 = 0.011;
const WAX_GLOW: f32 = 0.35;

fn mul(a: Vec3f, b: Vec3f) -> Vec3f {
    vec3f(a.x * b.x, a.y * b.y, a.z * b.z)
}

fn lum(c: Vec3f) -> f32 {
    0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z
}

fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Schlick's Fresnel for reflectance `f0` at cosine `c`.
fn fresnel(f0: Vec3f, c: f32) -> Vec3f {
    let k = (1.0 - c.clamp(0.0, 1.0)).powi(5);
    f0 + (Vec3f::all(1.0) - f0) * k
}

/// What a face is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mat {
    /// The candle's wax (the wax colour, glowing below its top).
    Wax,
    /// The molten pool on top (glossy, the wax colour a little deeper).
    Pool,
    /// The wick (black, its tip an ember).
    Wick,
    /// A tea light's aluminium cup.
    Cup,
    /// A votive's glass.
    Glass,
    /// The holder's metal (`CandleMetal`).
    Metal,
    /// A table's top or a pedestal's stone: the surface colour and gloss.
    Top,
    /// A table's base: dark painted metal.
    Base,
}

/// One flame: its foot (metres, the model's frame), height, width, lean (where its tip goes, metres, level) and
/// brightness, and its intensity (rel units x m2: its mean radiance times the area it shows from the side).
#[derive(Clone, Copy, Debug)]
struct Flame {
    foot: Vec3f,
    tall: f32,
    wide: f32,
    lean: Vec3f,
    bright: f32,
    intensity: f32,
    /// Where its light is taken to come from (40 % up its axis).
    centre: Vec3f,
}

/// The teardrop's radius at height share `t` over its widest (0 at foot and tip, 1 at 36 % up).
fn tear(t: f32) -> f32 {
    if !(t > 0.0 && t < 1.0) {
        return 0.0;
    }
    t.sqrt() * (1.0 - t).powf(0.9) / 0.4015
}

/// The emission (rel colour, plain colour) at height share `t` and radial share `s` of a flame of mean 1, before its
/// normalisation: `ratio` maps a Kelvin to its ratio to the light's colour.
fn emission(t: f32, s: f32, ratio: &dyn Fn(f32) -> Vec3f) -> (Vec3f, Vec3f) {
    if !(s < 1.0) || !(t > 0.0 && t < 1.0) {
        return (Vec3f::default(), Vec3f::default());
    }
    let edge = smooth(1.0, 0.55, s);
    // the luminous zone: weak at the foot, full from a fifth up, fading out over the top 40 %
    let along = (0.18 + 0.82 * smooth(0.0, 0.2, t)) * (1.0 - smooth(0.55, 1.0, t)).powf(0.8);
    // the wick's dark inner cone at the foot
    let cone = 1.0 - 0.7 * smooth(0.5, 0.15, s) * smooth(0.38, 0.12, t);
    let k = edge * along * cone;
    // whiter in the middle, deeper orange toward the edge and the tip
    let warm = (s * s * 0.8 + smooth(0.45, 1.0, t) * 0.9).min(1.0);
    let kelvin = CORE_KELVIN + (EDGE_KELVIN - CORE_KELVIN) * warm;
    // the blue skirt: the outer shell of the lowest sixth
    let blue = smooth(0.55, 0.9, s) * smooth(1.0, 0.85, s) * smooth(0.2, 0.03, t) * smooth(0.0, 0.03, t);
    (ratio(kelvin) * k, vec3f(0.12, 0.2, 0.75) * (0.35 * blue))
}

/// A ray's segment through the vertical cylinder of radius `r` round the line through `c` (only x and z read),
/// between heights `y0` and `y1`, clipped to `[0, t_max]`.
fn cylinder_span(o: Vec3f, d: Vec3f, c: Vec3f, r: f32, y0: f32, y1: f32, t_max: f32) -> Option<(f32, f32)> {
    let (ox, oz) = (o.x - c.x, o.z - c.z);
    let a = d.x * d.x + d.z * d.z;
    let (mut t0, mut t1) = (0.0f32, t_max);
    if a > 1.0e-12 {
        let b = ox * d.x + oz * d.z;
        let cc = ox * ox + oz * oz - r * r;
        let disc = b * b - a * cc;
        if disc < 0.0 {
            return None;
        }
        let q = disc.sqrt();
        t0 = t0.max((-b - q) / a);
        t1 = t1.min((-b + q) / a);
    } else if ox * ox + oz * oz > r * r {
        return None;
    }
    if d.y.abs() > 1.0e-9 {
        let (ta, tb) = ((y0 - o.y) / d.y, (y1 - o.y) / d.y);
        t0 = t0.max(ta.min(tb));
        t1 = t1.min(ta.max(tb));
    } else if o.y < y0 || o.y > y1 {
        return None;
    }
    (t1 > t0).then_some((t0, t1))
}

impl Flame {
    /// The flame's light along the ray `o + t d` (unit `d`) for t in [0, t_max], widened by `blur` metres (a texel's
    /// footprint, a rough face's lobe) keeping its energy: (rel, plain).
    fn along(&self, o: Vec3f, d: Vec3f, t_max: f32, blur: f32, ratio: &dyn Fn(f32) -> Vec3f) -> (Vec3f, Vec3f) {
        let w = (0.5 * self.wide).hypot(blur);
        let tall = self.tall + 2.0 * blur;
        let spread = (0.5 * self.wide / w) * (self.tall / tall);
        let lean = self.lean.x.abs().max(self.lean.z.abs());
        let Some((t0, t1)) = cylinder_span(o, d, self.foot + self.lean * 0.5, w + 0.5 * lean, self.foot.y - blur, self.foot.y - blur + tall, t_max) else {
            return (Vec3f::default(), Vec3f::default());
        };
        let step = (t1 - t0) / FLAME_STEPS as f32;
        let (mut rel, mut plain) = (Vec3f::default(), Vec3f::default());
        for i in 0..FLAME_STEPS {
            let p = o + d * (t0 + (i as f32 + 0.5) * step);
            let t = (p.y - (self.foot.y - blur)) / tall;
            let axis = self.foot + self.lean * (t * t);
            let (dx, dz) = (p.x - axis.x, p.z - axis.z);
            let r = w * tear(t);
            if !(r > 1.0e-7) {
                continue;
            }
            let s = (dx * dx + dz * dz).sqrt() / r;
            let (e, b) = emission(t, s, ratio);
            rel += e;
            plain += b;
        }
        // the density's scale: a chord through the widest middle of the flame (2 w) reads about 2.2 x the mean
        let k = self.bright * spread * step * FLAME_DENSITY / (0.5 * self.wide).max(1.0e-5);
        (rel * k, plain * k)
    }

    /// Its faint halo along the ray: by how near the ray passes its middle (in front of `t_max`).
    fn halo(&self, o: Vec3f, d: Vec3f, t_max: f32) -> f32 {
        let c = self.foot + vec3f(0.0, 0.45 * self.tall, 0.0) + self.lean * 0.2;
        let t = (c - o).dot(d);
        if !(t > 0.0 && t < t_max) {
            return 0.0;
        }
        let miss = (o + d * t - c).length();
        let reach = HALO_REACH * self.wide;
        let x = miss / reach;
        if x > 6.0 {
            return 0.0;
        }
        HALO * self.bright * (-x).exp() * smooth(6.0, 3.0, x)
    }
}

/// From this roughness up a face shows the flames as GGX highlights, below it their mirrored image.
const GLOSSY_ROUGH: f32 = 0.05;

/// The share of a flame's light that comes from below height share `t` (its light along its axis, a smooth bump
/// peaking 40 % up: `CandleModel::flame_light`).
fn flame_cdf(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    // the density 6 t (1 - t) skewed down: t -> t^0.8
    let u = t.powf(0.8);
    u * u * (3.0 - 2.0 * u)
}

/// `flame_cdf`'s inverse.
fn flame_cdf_inverse(share: f32) -> f32 {
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..16 {
        let mid = 0.5 * (lo + hi);
        if flame_cdf(mid) < share {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// The share of a flame's intensity a fully frosted votive's wall gives out sideways (0.64 x the 0.6 of the flame's
/// light its wall catches, half of what it scatters leaving outward).
const VOTIVE_GLOW: f32 = 0.38;

/// The emission density's scale, per half width (set so a flame's mean radiance over its side view is 1:
/// `flame_density_is_unit_mean` checks it).
const FLAME_DENSITY: f32 = 1.46;

/// The scene's light on the model (set once the scene is measured; `Studio::light_candles`).
#[derive(Clone, Copy, Debug, Default)]
pub enum CandleLight {
    /// Nothing measured: only the flames light it.
    #[default]
    Dark,
    /// Irradiance on faces turned along +x, -x, +y, -y, +z, -z of the map's frame (a room's light, a studio's
    /// backdrop).
    Cube([Vec3f; 6]),
    /// Outdoors: the floor's light (`FloorLighting`) with its key's direction (map frame), the ground's albedo.
    Sky { lit: FloorLighting, key: Option<Vec3f>, ground: Vec3f },
}

impl CandleLight {
    /// The irradiance on a face of normal `n` (unit, the map's frame).
    fn irradiance(&self, n: Vec3f) -> Vec3f {
        match self {
            CandleLight::Dark => Vec3f::default(),
            CandleLight::Cube(c) => {
                let pick = |v: f32, pos: Vec3f, neg: Vec3f| if v >= 0.0 { pos * (v * v) } else { neg * (v * v) };
                pick(n.x, c[0], c[1]) + pick(n.y, c[2], c[3]) + pick(n.z, c[4], c[5])
            }
            CandleLight::Sky { lit, key, ground } => {
                let a = [n.x, n.y, n.z];
                let k = key.map_or(0.0, |k| n.dot(k).max(0.0));
                let (bd, bn) = lit.bounce(a, *ground);
                lit.day(a, k) + lit.night(a, k) + bd + bn
            }
        }
    }
}

/// What a ray meets on a candle light (see the module's head).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CandleHit {
    pub rel: Vec3f,
    pub plain: Vec3f,
    pub abs: Vec3f,
    /// How much of the map behind it the ray no longer sees (1 a solid, about 0.1 clear glass, 0 a flame alone).
    pub opacity: f32,
}

/// A material's look.
struct Look {
    albedo: Vec3f,
    f0: Vec3f,
    rough: f32,
}

/// The model: its triangles, their normals, materials and owners (the candle each belongs to, for the wax's glow),
/// the flames, the candles' wax tops, and how it is lit.
pub struct CandleModel {
    bvh: TriBvh,
    /// What can stand between a face and a flame: every triangle but a table's or pedestal's (the flames stand over
    /// its top: `support`), with each one's material.
    shadow_bvh: TriBvh,
    shadow_mats: Vec<Mat>,
    /// A table's or pedestal's top height: nothing under it sees a flame.
    support: Option<f32>,
    normals: Vec<[Vec3f; 3]>,
    mats: Vec<Mat>,
    owner: Vec<u16>,
    flames: Vec<Flame>,
    /// Per candle: its axis's x and z, its wax top's height, its radius.
    tops: Vec<(f32, f32, f32, f32)>,
    /// The votives' glowing walls (`Votive`).
    votives: Vec<Votive>,
    wax: Vec3f,
    glass: Vec3f,
    frost: f32,
    metal: Look,
    top: Look,
    pool: bool,
    /// The bounding sphere (model frame): middle and radius, the halo and pool included.
    pub bound: (Vec3f, f32),
    /// The ratio of a Kelvin's colour to the light's own (a table over 1300..2900 K in 100 K steps).
    ratios: Vec<Vec3f>,
}

impl std::fmt::Debug for CandleModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CandleModel({} triangles, {} flames)", self.mats.len(), self.flames.len())
    }
}

/// The triangles being made.
#[derive(Default)]
struct Build {
    corners: Vec<[Vec3f; 3]>,
    normals: Vec<[Vec3f; 3]>,
    mats: Vec<Mat>,
    owner: Vec<u16>,
    flames: Vec<Flame>,
    tops: Vec<(f32, f32, f32, f32)>,
    votives: Vec<Votive>,
    who: u16,
}

/// A votive's wall as a light of its own: frosted glass takes a share of its flame's light and gives it out again
/// all round, so the table round a votive is lit by its glowing wall where the cup's rim keeps the flame itself
/// from it. Its axis's foot, the wall's height and radius, and its flame.
#[derive(Clone, Copy, Debug)]
struct Votive {
    foot: Vec3f,
    tall: f32,
    radius: f32,
    flame: usize,
}

impl Build {
    fn tri(&mut self, p: [Vec3f; 3], n: [Vec3f; 3], mat: Mat) {
        self.corners.push(p);
        self.normals.push(n);
        self.mats.push(mat);
        self.owner.push(self.who);
    }

    fn quad(&mut self, p: [Vec3f; 4], n: [Vec3f; 4], mat: Mat) {
        self.tri([p[0], p[1], p[2]], [n[0], n[1], n[2]], mat);
        self.tri([p[0], p[2], p[3]], [n[0], n[2], n[3]], mat);
    }

    /// A surface of revolution round the vertical axis through `at`: the profile's (radius, height) points in order
    /// from the bottom middle out and up (its outside on the right as it is walked), each segment with its own
    /// material. A corner sharper than 35 degrees is kept sharp.
    fn lathe(&mut self, at: Vec3f, profile: &[(f32, f32, Mat)]) {
        // a long segment (a table's top from its middle to its rim) is cut into rings, so no triangle is a long
        // sliver whose box every ray near it has to open
        let mut fine: Vec<(f32, f32, Mat)> = Vec::with_capacity(profile.len());
        for (i, &p) in profile.iter().enumerate() {
            if let Some(&(r0, y0, m0)) = i.checked_sub(1).map(|k| &profile[k]) {
                let len = (p.0 - r0).hypot(p.1 - y0);
                let cuts = (len / LATHE_STEP).ceil() as usize;
                for k in 1..cuts {
                    let t = k as f32 / cuts as f32;
                    fine.push((r0 + (p.0 - r0) * t, y0 + (p.1 - y0) * t, m0));
                }
            }
            fine.push(p);
        }
        let profile = &fine[..];
        let n = profile.len();
        if n < 2 {
            return;
        }
        // more sides round a wide one (a table's top), so its rim stays round
        let widest = profile.iter().map(|p| p.0).fold(0.0f32, f32::max);
        let sides = ((widest * 300.0) as usize).clamp(LATHE_SIDES, 160);
        // each segment's normal in the (r, y) plane
        let seg: Vec<(f32, f32)> = (0..n - 1)
            .map(|i| {
                let (dr, dy) = (profile[i + 1].0 - profile[i].0, profile[i + 1].1 - profile[i].1);
                let l = (dr * dr + dy * dy).sqrt().max(1.0e-9);
                (dy / l, -dr / l)
            })
            .collect();
        let smooth_at = |i: usize, s: usize| -> (f32, f32) {
            // vertex i's normal for segment s: the mean of its two segments' unless the corner is sharp
            let other = if i == s { s.checked_sub(1) } else { (s + 1 < seg.len()).then_some(s + 1) };
            let a = seg[s];
            match other {
                Some(o) if a.0 * seg[o].0 + a.1 * seg[o].1 > 0.82 => {
                    let (x, y) = (a.0 + seg[o].0, a.1 + seg[o].1);
                    let l = (x * x + y * y).sqrt().max(1.0e-9);
                    (x / l, y / l)
                }
                _ => a,
            }
        };
        for s in 0..n - 1 {
            let (r0, y0, mat) = profile[s];
            let (r1, y1, _) = profile[s + 1];
            if (r0 - r1).abs() < 1.0e-7 && (y0 - y1).abs() < 1.0e-7 {
                continue;
            }
            let (n0, n1) = (smooth_at(s, s), smooth_at(s + 1, s));
            for k in 0..sides {
                let a0 = std::f32::consts::TAU * k as f32 / sides as f32;
                let a1 = std::f32::consts::TAU * (k + 1) as f32 / sides as f32;
                let (c0, s0, c1, s1) = (a0.cos(), a0.sin(), a1.cos(), a1.sin());
                let p = |r: f32, y: f32, c: f32, s: f32| at + vec3f(r * c, y, r * s);
                let nn = |n: (f32, f32), c: f32, s: f32| vec3f(n.0 * c, n.1, n.0 * s);
                self.quad(
                    [p(r0, y0, c0, s0), p(r0, y0, c1, s1), p(r1, y1, c1, s1), p(r1, y1, c0, s0)],
                    [nn(n0, c0, s0), nn(n0, c1, s1), nn(n1, c1, s1), nn(n1, c0, s0)],
                    mat,
                );
            }
        }
    }

    /// A tube of radius `r` along the polyline `pts`, `sides` round, its ends open (they meet a cup or a boss).
    fn tube(&mut self, pts: &[Vec3f], r: f32, sides: usize, mat: Mat) {
        if pts.len() < 2 {
            return;
        }
        let mut prev_e1: Option<Vec3f> = None;
        let mut rings: Vec<Vec<(Vec3f, Vec3f)>> = Vec::with_capacity(pts.len());
        for i in 0..pts.len() {
            let t = if i == 0 { pts[1] - pts[0] } else if i + 1 == pts.len() { pts[i] - pts[i - 1] } else { pts[i + 1] - pts[i - 1] };
            let t = t.normalize();
            // a frame carried along the line (no twist)
            let e1 = match prev_e1 {
                Some(e) => (e - t * e.dot(t)).normalize(),
                None => (if t.y.abs() < 0.9 { Vec3f::cross(t, vec3f(0.0, 1.0, 0.0)) } else { Vec3f::cross(t, vec3f(1.0, 0.0, 0.0)) }).normalize(),
            };
            prev_e1 = Some(e1);
            let e2 = Vec3f::cross(t, e1);
            rings.push(
                (0..=sides)
                    .map(|k| {
                        let a = std::f32::consts::TAU * k as f32 / sides as f32;
                        let n = e1 * a.cos() + e2 * a.sin();
                        (pts[i] + n * r, n)
                    })
                    .collect(),
            );
        }
        for i in 0..rings.len() - 1 {
            for k in 0..sides {
                let (a, b, c, d) = (rings[i][k], rings[i][k + 1], rings[i + 1][k + 1], rings[i + 1][k]);
                self.quad([a.0, b.0, c.0, d.0], [a.1, b.1, c.1, d.1], mat);
            }
        }
    }

    /// A flat box from `lo` to `hi` (a wall plate, a pedestal's slab).
    fn cuboid(&mut self, lo: Vec3f, hi: Vec3f, mat: Mat) {
        let p = |i: usize| vec3f(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z });
        let faces: [([usize; 4], Vec3f); 6] = [
            ([0, 2, 6, 4], vec3f(-1.0, 0.0, 0.0)),
            ([1, 5, 7, 3], vec3f(1.0, 0.0, 0.0)),
            ([0, 4, 5, 1], vec3f(0.0, -1.0, 0.0)),
            ([2, 3, 7, 6], vec3f(0.0, 1.0, 0.0)),
            ([0, 1, 3, 2], vec3f(0.0, 0.0, -1.0)),
            ([4, 6, 7, 5], vec3f(0.0, 0.0, 1.0)),
        ];
        for (c, n) in faces {
            self.quad([p(c[0]), p(c[1]), p(c[2]), p(c[3])], [n; 4], mat);
        }
    }
}

/// A smooth curve through `pts` (Catmull-Rom), `per` points per span.
fn spline(pts: &[Vec3f], per: usize) -> Vec<Vec3f> {
    let n = pts.len();
    if n < 3 {
        return pts.to_vec();
    }
    let at = |i: isize| pts[i.clamp(0, n as isize - 1) as usize];
    let mut out = Vec::new();
    for i in 0..n - 1 {
        let (p0, p1, p2, p3) = (at(i as isize - 1), at(i as isize), at(i as isize + 1), at(i as isize + 2));
        for k in 0..per {
            let t = k as f32 / per as f32;
            let (t2, t3) = (t * t, t * t * t);
            out.push((p1 * 2.0 + (p2 - p0) * t + (p0 * 2.0 - p1 * 5.0 + p2 * 4.0 - p3) * t2 + (p1 * 3.0 - p0 - p2 * 3.0 + p3) * t3) * 0.5);
        }
    }
    out.push(pts[n - 1]);
    out
}

/// One candle set down with its foot at `foot` (the middle of its base), its own flicker drawn from `seed`.
fn candle(b: &mut Build, c: &CandleParams, foot: Vec3f, seed: u32) {
    let kind = c.kind();
    let (r, h_new) = (0.5 * c.diameter_m, c.height_m);
    let burn = c.burn;
    let wax = Mat::Wax;
    // the top of the molten pool, where the wick stands
    let pool_y;
    match kind {
        CandleKind::TeaLight => {
            // the cup (thin aluminium, drawn as one face), the wax inside it sinking as it burns
            let cup_h = h_new.max(0.008);
            let wax_y = 0.0025 + (cup_h - 0.004) * (1.0 - burn);
            b.lathe(foot, &[(0.0, 0.0, Mat::Cup), (r - 0.0012, 0.0, Mat::Cup), (r, 0.0012, Mat::Cup), (r, cup_h, Mat::Cup), (r - 0.0006, cup_h, Mat::Cup), (r - 0.0006, wax_y, Mat::Pool), (0.0, wax_y, Mat::Pool)]);
            pool_y = wax_y;
        }
        CandleKind::Pillar => {
            // the wall stands round a well the flame has melted; a new pillar is flat on top
            let h = h_new * (1.0 - burn).max(0.05);
            let well = (burn * 0.9).min(1.0) * (0.25 * r).min(0.025);
            let wall = (0.18 * r).max(0.004);
            let lip = (0.12 * r).min(0.006);
            pool_y = h - well - 0.002;
            b.lathe(
                foot,
                &[
                    (0.0, 0.0, wax),
                    (r - 0.003, 0.0, wax),
                    (r, 0.003, wax),
                    (r, h - lip, wax),
                    (r - 0.3 * lip, h - 0.15 * lip, wax),
                    (r - lip, h, wax),
                    (r - wall, h - 0.3 * well, wax),
                    (r - 1.3 * wall, pool_y, Mat::Pool),
                    (0.0, pool_y, Mat::Pool),
                ],
            );
        }
        CandleKind::Taper => {
            // a taper narrows a little to its top; new its top is a low dome, burnt a small cup
            let h = h_new * (1.0 - burn).max(0.05);
            let rt = r * 0.86;
            let ri = |y: f32| r + (rt - r) * (y / h_new);
            pool_y = if burn > 0.02 { h - 0.002 } else { h + 0.3 * rt };
            if burn > 0.02 {
                b.lathe(foot, &[(0.0, 0.0, wax), (ri(0.0), 0.0, wax), (ri(h - 0.002), h - 0.002, wax), (ri(h) - 0.0012, h, wax), (0.5 * ri(h), h - 0.0015, Mat::Pool), (0.0, h - 0.002, Mat::Pool)]);
            } else {
                b.lathe(foot, &[(0.0, 0.0, wax), (ri(0.0), 0.0, wax), (ri(h), h, wax), (0.8 * rt, h + 0.18 * rt, wax), (0.45 * rt, h + 0.28 * rt, wax), (0.0, h + 0.3 * rt, wax)]);
            }
        }
    }
    let rnd = |k: u32| hash01(seed as i32, k as i32, 17, 0x5ca1_ab1e);
    let flicker = c.flicker;
    // the wick: a short black thread, bent a little, its tip in the flame's dark cone
    let (tall, wide) = c.flame_size();
    let tall = tall * (1.0 + flicker * 0.45 * (rnd(1) - 0.5));
    let wide = wide * (1.0 + flicker * 0.2 * (rnd(2) - 0.5));
    let bend = rnd(3) * std::f32::consts::TAU;
    let lean_amount = flicker * tall * 0.45 * rnd(4).powf(1.5);
    let lean = vec3f(bend.cos() * lean_amount, 0.0, bend.sin() * lean_amount);
    let wick_r = if kind == CandleKind::Pillar { 0.0009 } else { 0.0007 };
    let wick_top = 0.25 * tall + 0.003;
    let top = foot + vec3f(0.0, pool_y, 0.0);
    let curl = vec3f(bend.cos(), 0.0, bend.sin()) * (0.25 * wick_top);
    b.tube(&[top - vec3f(0.0, 0.002, 0.0), top + vec3f(0.0, 0.5 * wick_top, 0.0) + curl * 0.3, top + vec3f(0.0, wick_top, 0.0) + curl], wick_r, WICK_SIDES, Mat::Wick);
    let flame_foot = top + vec3f(0.0, 0.0015, 0.0) + curl * 0.4;
    let bright = 1.0 + flicker * 0.3 * (rnd(5) - 0.5);
    b.flames.push(Flame { foot: flame_foot, tall, wide, lean, bright, intensity: 0.0, centre: flame_foot + vec3f(0.0, 0.4 * tall, 0.0) + lean * 0.16 });
    b.tops.push((foot.x, foot.z, pool_y + foot.y, r));
}

/// A votive's glass round a candle standing at `foot`: a thick-bottomed straight cup a little wider and taller than
/// the candle (both its faces).
fn votive(b: &mut Build, c: &CandleParams, foot: Vec3f) -> Vec3f {
    let r_in = 0.5 * c.diameter_m + 0.008;
    let wall = 0.0028;
    let base = 0.007;
    let top = (c.height_m + 0.04).max(0.068).min(0.4);
    let r_out = r_in + wall;
    b.votives.push(Votive { foot, tall: top, radius: r_out, flame: b.flames.len() });
    b.lathe(
        foot,
        &[
            (0.0, 0.0, Mat::Glass),
            (r_out - 0.003, 0.0, Mat::Glass),
            (r_out, 0.003, Mat::Glass),
            (r_out, top - 0.0012, Mat::Glass),
            (r_out - 0.0012, top, Mat::Glass),
            (r_in + 0.0004, top, Mat::Glass),
            (r_in, top - 0.0012, Mat::Glass),
            (r_in, base + 0.002, Mat::Glass),
            (r_in - 0.002, base, Mat::Glass),
            (0.0, base, Mat::Glass),
        ],
    );
    foot + vec3f(0.0, base, 0.0)
}

/// A turned candlestick `tall` metres high standing at `foot`: base, stem with knops, drip pan and socket. Where the
/// candle stands in its socket.
fn candlestick(b: &mut Build, c: &CandleParams, foot: Vec3f, tall: f32) -> Vec3f {
    let rc = 0.5 * c.diameter_m;
    let s = tall / 0.225;
    let m = Mat::Metal;
    let socket = (rc + 0.003).max(0.012);
    let p = [
        (0.0, 0.0),
        (0.056, 0.0),
        (0.058, 0.004),
        (0.052, 0.011),
        (0.034, 0.018),
        (0.016, 0.03),
        (0.012, 0.05),
        (0.019, 0.066),
        (0.012, 0.08),
        (0.0095, 0.15),
        (0.016, 0.165),
        (0.0105, 0.18),
        (0.034, 0.19),
        (0.036, 0.195),
        (0.0, 0.0),
    ];
    let mut prof: Vec<(f32, f32, Mat)> = p[..p.len() - 1].iter().map(|&(r, y)| (r, y * s, m)).collect();
    let pan = 0.195 * s;
    prof.push((socket + 0.0035, pan + 0.004, m));
    prof.push((socket + 0.0025, pan + 0.024, m));
    prof.push((socket, pan + 0.024, m));
    prof.push((socket, pan + 0.006, m));
    prof.push((0.0, pan + 0.006, m));
    b.lathe(foot, &prof);
    foot + vec3f(0.0, pan + 0.006, 0.0)
}

/// A cup (a drip pan and socket) for a candle at `at`, its bottom there: where the candle stands.
fn cup(b: &mut Build, c: &CandleParams, at: Vec3f) -> Vec3f {
    let socket = (0.5 * c.diameter_m + 0.003).max(0.012);
    let m = Mat::Metal;
    b.lathe(at, &[(0.0, -0.012, m), (0.012, -0.012, m), (0.007, -0.004, m), (0.032, 0.0, m), (0.034, 0.004, m), (socket + 0.003, 0.006, m), (socket + 0.0025, 0.026, m), (socket, 0.026, m), (socket, 0.008, m), (0.0, 0.008, m)]);
    at + vec3f(0.0, 0.008, 0.0)
}

/// A candelabra standing at `foot`: a turned stem, its candle in the middle, and `arms` S-curved arms each with its
/// cup (two in a line across the view, more round the stem).
fn candelabra(b: &mut Build, c: &CandleParams, foot: Vec3f, seed: u32) {
    let tall = 0.34;
    let middle = candlestick(b, c, foot, tall);
    b.who = b.tops.len() as u16;
    candle(b, c, middle, seed);
    let arms = c.arms.max(2) as usize;
    let reach = 0.17 + 0.012 * arms as f32;
    let start_y = 0.19;
    for k in 0..arms {
        let a = if arms == 2 { if k == 0 { 0.0 } else { std::f32::consts::PI } } else { std::f32::consts::TAU * k as f32 / arms as f32 + 0.5 * std::f32::consts::PI };
        let out = vec3f(a.cos(), 0.0, a.sin());
        let pts = [
            foot + vec3f(0.0, start_y, 0.0) + out * 0.012,
            foot + vec3f(0.0, start_y - 0.035, 0.0) + out * (0.35 * reach),
            foot + vec3f(0.0, start_y - 0.02, 0.0) + out * (0.75 * reach),
            foot + vec3f(0.0, start_y + 0.04, 0.0) + out * reach,
            foot + vec3f(0.0, start_y + 0.075, 0.0) + out * reach,
        ];
        b.tube(&spline(&pts, 6), 0.0065, TUBE_SIDES, Mat::Metal);
        let at = cup(b, c, foot + vec3f(0.0, start_y + 0.087, 0.0) + out * reach);
        b.who = b.tops.len() as u16;
        candle(b, c, at, seed.wrapping_add(k as u32 * 7 + 3));
    }
}

/// A chandelier hanging `drop` metres from the ceiling point at `top` (the model's origin): the ceiling's rose, a
/// rod, the boss, and `arms` arms curving out and up to their cups.
fn chandelier(b: &mut Build, c: &CandleParams, top: Vec3f, drop: f32, seed: u32) {
    let m = Mat::Metal;
    let drop = drop.max(0.25);
    // the ceiling's rose and the rod (with a few links' worth of beads)
    b.lathe(top, &[(0.0, -0.03, m), (0.04, -0.03, m), (0.055, -0.01, m), (0.06, 0.0, m), (0.0, 0.0, m)]);
    let boss_top = top - vec3f(0.0, drop - 0.12, 0.0);
    b.tube(&[top - vec3f(0.0, 0.03, 0.0), boss_top], 0.006, TUBE_SIDES, m);
    let beads = ((drop - 0.15) / 0.12).floor().max(0.0) as usize;
    for i in 0..beads {
        let y = top.y - 0.08 - 0.12 * i as f32;
        b.lathe(vec3f(top.x, y, top.z), &[(0.0, -0.012, m), (0.01, -0.008, m), (0.012, 0.0, m), (0.01, 0.008, m), (0.0, 0.012, m)]);
    }
    // the boss: a turned vase, a finial under it
    let bottom = boss_top - vec3f(0.0, 0.2, 0.0);
    b.lathe(bottom, &[(0.0, -0.06, m), (0.008, -0.05, m), (0.014, -0.02, m), (0.02, 0.0, m), (0.05, 0.03, m), (0.06, 0.06, m), (0.05, 0.1, m), (0.022, 0.13, m), (0.018, 0.16, m), (0.03, 0.18, m), (0.014, 0.2, m), (0.0, 0.2, m)]);
    let arms = c.arms.max(2) as usize;
    let reach = 0.26 + 0.02 * arms as f32;
    for k in 0..arms {
        let a = std::f32::consts::TAU * (k as f32 + 0.5) / arms as f32;
        let out = vec3f(a.cos(), 0.0, a.sin());
        let pts = [
            bottom + vec3f(0.0, 0.045, 0.0) + out * 0.05,
            bottom + vec3f(0.0, -0.02, 0.0) + out * (0.3 * reach),
            bottom + vec3f(0.0, -0.04, 0.0) + out * (0.65 * reach),
            bottom + vec3f(0.0, 0.02, 0.0) + out * (0.95 * reach),
            bottom + vec3f(0.0, 0.09, 0.0) + out * reach,
        ];
        b.tube(&spline(&pts, 6), 0.0075, TUBE_SIDES, m);
        let at = cup(b, c, bottom + vec3f(0.0, 0.102, 0.0) + out * reach);
        b.who = b.tops.len() as u16;
        candle(b, c, at, seed.wrapping_add(k as u32 * 11 + 5));
    }
}

/// A sconce on the wall at the origin: an oval back plate in the wall's plane (z = 0), as tall as its candle stands,
/// and an arm from its lower part out to a cup at its foot, so the candle stands in front of the plate and its flame
/// lights the plate's upper half (`count` candles, at most 3, side by side, each on its own arm).
fn sconce(b: &mut Build, c: &CandleParams, seed: u32) {
    let m = Mat::Metal;
    let n = c.count.clamp(1, 3) as usize;
    let visible = match c.kind() {
        CandleKind::TeaLight => 0.03,
        _ => c.height_m * (1.0 - c.burn).max(0.05),
    };
    let half_h = (0.5 * visible + 0.05).clamp(0.08, 0.3);
    let half_w = 0.06 + 0.045 * (n as f32 - 1.0);
    // the plate: a flat turned dish lying against the wall (its axis along +z), stretched to its oval
    let plate: Vec<(f32, f32, Mat)> = vec![(0.0, 0.0, m), (0.04, 0.0, m), (0.045, 0.004, m), (0.036, 0.011, m), (0.01, 0.016, m), (0.0, 0.018, m)];
    let (sx, sy) = (half_w / 0.045, half_h / 0.045);
    let mut sub = Build::default();
    sub.lathe(Vec3f::default(), &plate);
    for (tri, nrm) in sub.corners.iter().zip(&sub.normals) {
        let t = |p: Vec3f| vec3f(p.x * sx, p.z * sy, p.y);
        let tn = |v: Vec3f| vec3f(v.x / sx, v.z / sy, v.y).normalize();
        b.tri([t(tri[0]), t(tri[1]), t(tri[2])], [tn(nrm[0]), tn(nrm[1]), tn(nrm[2])], m);
    }
    let out = 0.11;
    let cup_y = -half_h + 0.01;
    for i in 0..n {
        let x = (i as f32 - 0.5 * (n as f32 - 1.0)) * 0.09;
        let pts = [vec3f(x * 0.5, -0.35 * half_h, 0.014), vec3f(x * 0.7, -0.6 * half_h, 0.04), vec3f(x * 0.9, cup_y - 0.07, 0.08), vec3f(x, cup_y - 0.05, out), vec3f(x, cup_y - 0.012, out)];
        b.tube(&spline(&pts, 6), 0.006, TUBE_SIDES, m);
        let at = cup(b, c, vec3f(x, cup_y, out));
        b.who = b.tops.len() as u16;
        candle(b, c, at, seed.wrapping_add(i as u32 * 13 + 1));
    }
}

/// Where a group of `count` stands round `at` laid out as `layout` (`group`'s scatter, a row across x, or a rack's
/// tiers, drawn here in the holder's metal: each tier a step back and up, a row of candles on each).
fn layout(b: &mut Build, at: Vec3f, layout: CandleLayout, count: usize, spread: f32, gap: f32, seed: u32) -> Vec<Vec3f> {
    match layout {
        CandleLayout::Scatter => group(at, count, spread, gap, seed),
        CandleLayout::Row => {
            if count <= 1 {
                return vec![at];
            }
            let half = spread.max(0.5 * gap * (count as f32 - 1.0));
            (0..count).map(|i| at + vec3f(-half + 2.0 * half * i as f32 / (count as f32 - 1.0), 0.0, 0.0)).collect()
        }
        CandleLayout::Rack => {
            // three tiers when there are enough candles, each 9 cm back and 9 cm up from the one before
            let tiers = if count >= 9 { 3 } else if count >= 4 { 2 } else { 1 };
            let per = (count + tiers - 1) / tiers;
            let half = spread.max(0.5 * gap * per as f32);
            let (step_up, step_back, depth) = (0.09, 0.09, 0.08);
            let m = Mat::Metal;
            b.who = u16::MAX;
            let mut out = Vec::with_capacity(count);
            for t in 0..tiers {
                let y = at.y + 0.18 + step_up * t as f32;
                let z = at.z + step_back * (tiers as f32 * 0.5 - t as f32);
                b.cuboid(vec3f(at.x - half - 0.03, y - 0.012, z - 0.5 * depth), vec3f(at.x + half + 0.03, y, z + 0.5 * depth), m);
                let here = per.min(count - out.len());
                for i in 0..here {
                    let x = if here == 1 { 0.0 } else { -half + 2.0 * half * (i as f32 + 0.5) / here as f32 };
                    out.push(vec3f(at.x + x, y, z));
                }
            }
            // its legs, and its back rail
            let (x0, x1) = (at.x - half - 0.02, at.x + half + 0.02);
            let top = at.y + 0.18 + step_up * (tiers as f32 - 1.0);
            let back = at.z - step_back * (tiers as f32 * 0.5 - 0.5) - 0.5 * depth;
            let front = at.z + step_back * tiers as f32 * 0.5 + 0.5 * depth;
            for x in [x0, x1] {
                b.tube(&[vec3f(x, at.y, front - 0.01), vec3f(x, at.y + 0.18, front - 0.01)], 0.008, TUBE_SIDES, m);
                b.tube(&[vec3f(x, at.y, back), vec3f(x, top + 0.02, back)], 0.008, TUBE_SIDES, m);
            }
            out
        }
    }
}

/// Where a group of `count` stands on a top round `at` within `spread` metres: a sunflower spiral, jittered.
fn group(at: Vec3f, count: usize, spread: f32, gap: f32, seed: u32) -> Vec<Vec3f> {
    if count <= 1 {
        return vec![at];
    }
    (0..count)
        .map(|i| {
            let r = spread * ((i as f32 + 0.5) / count as f32).sqrt();
            let a = i as f32 * 2.399_963 + seed as f32 * 0.73;
            let j = |k: i32| (hash01(i as i32, k, seed as i32, 0x9e37) - 0.5) * 0.35 * gap;
            at + vec3f(r * a.cos() + j(1), 0.0, r * a.sin() + j(2))
        })
        .collect()
}

impl CandleModel {
    /// The model of `c` (as clamp() holds it).
    pub fn new(c: &CandleParams) -> CandleModel {
        let c = &c.clamped();
        let mut b = Build::default();
        let seed = c.seed;
        let mount = c.mount();
        // a mount on the wall holds a sconce, one on the ceiling a chandelier; a sconce or chandelier asked for on
        // the floor, a table or a pedestal is a candelabra there
        let holder = match (mount, c.holder()) {
            (CandleMount::Wall, _) => CandleHolder::Sconce,
            (CandleMount::Ceiling, _) => CandleHolder::Chandelier,
            (_, CandleHolder::Sconce | CandleHolder::Chandelier) => CandleHolder::Candelabra,
            (_, h) => h,
        };
        // what the candles stand on
        let base = match mount {
            CandleMount::Table => {
                let h = c.mount_height_m.max(0.2);
                let r = 0.5 * c.mount_size_m;
                b.who = u16::MAX;
                b.lathe(Vec3f::default(), &[(0.0, h - 0.03, Mat::Top), (r - 0.006, h - 0.03, Mat::Top), (r, h - 0.024, Mat::Top), (r, h - 0.006, Mat::Top), (r - 0.006, h, Mat::Top), (0.0, h, Mat::Top)]);
                let foot = (0.5 * r).clamp(0.15, 0.35);
                b.lathe(Vec3f::default(), &[(0.0, 0.0, Mat::Base), (foot, 0.0, Mat::Base), (foot, 0.012, Mat::Base), (0.05, 0.03, Mat::Base), (0.045, h - 0.06, Mat::Base), (0.09, h - 0.035, Mat::Base), (0.0, h - 0.03, Mat::Base)]);
                vec3f(0.0, h, 0.0)
            }
            CandleMount::Pedestal => {
                let h = c.mount_height_m.max(0.1);
                let w = 0.5 * c.mount_size_m.min(1.5);
                b.who = u16::MAX;
                let plinth = (0.08f32).min(0.2 * h);
                b.cuboid(vec3f(-1.15 * w, 0.0, -1.15 * w), vec3f(1.15 * w, plinth, 1.15 * w), Mat::Top);
                b.cuboid(vec3f(-w, plinth, -w), vec3f(w, h - plinth, w), Mat::Top);
                b.cuboid(vec3f(-1.12 * w, h - plinth, -1.12 * w), vec3f(1.12 * w, h, 1.12 * w), Mat::Top);
                vec3f(0.0, h, 0.0)
            }
            _ => Vec3f::default(),
        };
        let gap = c.diameter_m + if holder == CandleHolder::Votive { 0.03 } else if holder == CandleHolder::Candlestick { 0.12 } else { 0.01 };
        match holder {
            CandleHolder::Sconce => sconce(&mut b, c, seed),
            CandleHolder::Chandelier => chandelier(&mut b, c, Vec3f::default(), c.mount_height_m, seed),
            CandleHolder::Candelabra => candelabra(&mut b, c, base, seed),
            CandleHolder::None | CandleHolder::Votive | CandleHolder::Candlestick => {
                let spread = if c.layout() == CandleLayout::Scatter { c.spread_m.max(0.55 * gap * (c.count as f32).sqrt()) } else { c.spread_m };
                for (i, at) in layout(&mut b, base, c.layout(), c.count as usize, spread, gap, seed).into_iter().enumerate() {
                    let s = seed.wrapping_mul(31).wrapping_add(i as u32);
                    let foot = match holder {
                        CandleHolder::Votive => votive(&mut b, c, at),
                        CandleHolder::Candlestick => candlestick(&mut b, c, at, 0.225),
                        _ => at,
                    };
                    b.who = b.tops.len() as u16;
                    candle(&mut b, c, foot, s);
                }
            }
        }
        let wax = vec3f(c.wax[0], c.wax[1], c.wax[2]);
        let glass = c.glass.map_or(Vec3f::all(1.0), |g| vec3f(g[0], g[1], g[2]));
        let metal = match c.metal() {
            CandleMetal::Brass => Look { albedo: vec3f(0.05, 0.035, 0.012), f0: vec3f(0.72, 0.54, 0.25), rough: 0.22 },
            CandleMetal::Iron => Look { albedo: vec3f(0.025, 0.024, 0.023), f0: vec3f(0.1, 0.1, 0.1), rough: 0.4 },
            CandleMetal::Silver => Look { albedo: vec3f(0.04, 0.04, 0.04), f0: vec3f(0.86, 0.86, 0.84), rough: 0.12 },
        };
        let top = Look { albedo: vec3f(c.surface[0], c.surface[1], c.surface[2]), f0: Vec3f::all(0.04), rough: 0.02 + 0.6 * (1.0 - c.gloss).powf(1.5) };
        let ratio0 = kelvin_to_rgb(crate::params::CANDLE_KELVIN);
        let ratios: Vec<Vec3f> = (0..17)
            .map(|i| {
                let k = kelvin_to_rgb(1300.0 + 100.0 * i as f32);
                vec3f(k.x / ratio0.x.max(1.0e-4), k.y / ratio0.y.max(1.0e-4), k.z / ratio0.z.max(1.0e-4))
            })
            .collect();
        // each flame's intensity: its mean radiance (1 x its brightness) over the area it shows from the side
        for f in &mut b.flames {
            let mut area = 0.0;
            for i in 0..64 {
                let t = (i as f32 + 0.5) / 64.0;
                area += f.wide * tear(t) * f.tall / 64.0;
            }
            f.intensity = f.bright * area;
        }
        // the bound: the triangles, the flames with their halos, a floor's pool
        let (mut lo, mut hi) = (Vec3f::all(f32::INFINITY), Vec3f::all(f32::NEG_INFINITY));
        for p in b.corners.iter().flatten() {
            lo = Vec3f::min_componentwise(lo, *p);
            hi = Vec3f::max_componentwise(hi, *p);
        }
        for f in &b.flames {
            let reach = 6.0 * HALO_REACH * f.wide;
            lo = Vec3f::min_componentwise(lo, f.foot - Vec3f::all(reach));
            hi = Vec3f::max_componentwise(hi, f.foot + vec3f(0.0, f.tall, 0.0) + Vec3f::all(reach));
        }
        let pool = mount == CandleMount::Floor;
        if pool {
            lo = Vec3f::min_componentwise(lo, vec3f(-POOL_REACH, 0.0, -POOL_REACH));
            hi = Vec3f::max_componentwise(hi, vec3f(POOL_REACH, 0.0, POOL_REACH));
        }
        let mid = (lo + hi) * 0.5;
        let radius = (hi - lo).length() * 0.5 + 1.0e-3;
        let (mut shadow_corners, mut shadow_mats) = (Vec::new(), Vec::new());
        for (c, m) in b.corners.iter().zip(&b.mats) {
            if !matches!(m, Mat::Top | Mat::Base) {
                shadow_corners.push(*c);
                shadow_mats.push(*m);
            }
        }
        let support = matches!(mount, CandleMount::Table | CandleMount::Pedestal).then_some(base.y);
        CandleModel {
            shadow_bvh: TriBvh::new(shadow_corners),
            shadow_mats,
            support,
            bvh: TriBvh::new(b.corners),
            normals: b.normals,
            mats: b.mats,
            owner: b.owner,
            flames: b.flames,
            tops: b.tops,
            votives: b.votives,
            wax,
            glass,
            frost: c.frost,
            metal,
            top,
            pool,
            bound: (mid, radius),
            ratios,
        }
    }

    pub fn triangles(&self) -> usize {
        self.mats.len()
    }

    /// The flames' feet, heights and intensities (rel units x m2), the model's frame: the room's lights.
    pub fn flames(&self) -> Vec<(Vec3f, f32)> {
        self.flames.iter().map(|f| (f.centre, f.intensity)).collect()
    }

    fn ratio(&self, kelvin: f32) -> Vec3f {
        let x = ((kelvin - 1300.0) / 100.0).clamp(0.0, 15.999);
        let i = x as usize;
        let t = x - i as f32;
        self.ratios[i] * (1.0 - t) + self.ratios[i + 1] * t
    }

    /// What the light passing from `p` toward `q` keeps through the model: 0 behind a solid, glass's tint per face.
    fn pass(&self, p: Vec3f, q: Vec3f) -> Vec3f {
        let d = q - p;
        let len = d.length();
        if !(len > 1.0e-6) {
            return Vec3f::all(1.0);
        }
        let d = d / len;
        let mut keep = Vec3f::all(1.0);
        self.shadow_bvh.each_hit(p + d * PAST, d, len - PAST, |tri, face| match self.shadow_mats[tri] {
            Mat::Glass => {
                keep = mul(keep, self.glass_pass(face.dot(d).abs()));
                true
            }
            Mat::Wick => true,
            _ => {
                keep = Vec3f::default();
                false
            }
        });
        keep
    }

    /// One glass face crossed at cosine `c`: what it lets through (its Fresnel's rest and, along the path through
    /// its wall, its tint).
    fn glass_pass(&self, c: f32) -> Vec3f {
        let f = fresnel(Vec3f::all(0.04), c).x;
        let tint = vec3f(self.glass.x.powf(0.5), self.glass.y.powf(0.5), self.glass.z.powf(0.5));
        // frost scatters what it does not pass straight on (its glow, `VOTIVE_GLOW`)
        tint * ((1.0 - f) * (1.0 - 0.35 * self.frost))
    }

    /// The flames' light (rel) on a face at `p` facing `n` (unit): each by its cosine and its intensity over its
    /// distance squared, through the model.
    ///
    /// A flame is as tall as a tea light's cup is deep, so it is not one point but a short glowing line up its axis.
    /// What hides it from `p` (a cup's rim, a candlestick's pan, the wax's own wall) hides its lower part first, so
    /// the lowest point of the line `p` still sees is found by bisection and the share of the flame's light above it
    /// arrives (`flame_cdf`), from the middle of that visible part: a soft shadow edge, with two looks where nothing
    /// is in the way.
    fn flame_light(&self, p: Vec3f, n: Vec3f) -> Vec3f {
        self.flame_light_glossy(p, n, None).0
    }

    /// `flame_light`, and with `glossy` (the view's unit direction toward the face, its roughness) the flames'
    /// highlight on it: GGX's lobe round each flame's visible middle, as a point (no Fresnel).
    fn flame_light_glossy(&self, p: Vec3f, n: Vec3f, glossy: Option<(Vec3f, f32)>) -> (Vec3f, Vec3f) {
        let mut e = Vec3f::default();
        let mut highlight = Vec3f::default();
        // under a table's or pedestal's top no flame is seen
        if self.support.is_some_and(|y| p.y < y - 1.0e-3) {
            return (e, highlight);
        }
        let from = p + n * 1.0e-4;
        for f in &self.flames {
            let at = |t: f32| f.foot + vec3f(0.0, t * f.tall, 0.0) + f.lean * (t * t);
            let (lo_t, hi_t) = (0.08, 0.92);
            let top = self.pass(from, at(hi_t));
            if top == Vec3f::default() {
                continue;
            }
            let bottom = self.pass(from, at(lo_t));
            // the lowest visible height share: lo_t when all of it shows, else bisected
            let cut = if bottom != Vec3f::default() {
                0.0
            } else {
                let (mut lo, mut hi) = (lo_t, hi_t);
                for _ in 0..5 {
                    let mid = 0.5 * (lo + hi);
                    if self.pass(from, at(mid)) == Vec3f::default() {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                0.5 * (lo + hi)
            };
            let share = 1.0 - flame_cdf(cut);
            if !(share > 0.0) {
                continue;
            }
            // the visible part's middle (by its light), and the light from there
            let c = at(flame_cdf_inverse(1.0 - 0.5 * share));
            let l = c - p;
            let d2 = l.length_squared().max(1.0e-6);
            let cos = n.dot(l) / d2.sqrt();
            if !(cos > 0.0) {
                continue;
            }
            e += top * (share * f.intensity * cos / d2);
            if let Some((d, rough)) = glossy {
                let cos_v = (-d.dot(n)).max(1.0e-3);
                let w = l / d2.sqrt();
                let h = (w - d).normalize();
                let a2 = (rough * rough).max(1.0e-4);
                let nh = n.dot(h).max(0.0);
                let den = nh * nh * (a2 - 1.0) + 1.0;
                let ggx = a2 / (std::f32::consts::PI * den * den);
                highlight += top * (share * f.intensity * ggx / (4.0 * d2 * cos_v));
            }
        }
        // the frosted votives' walls: what the glass scatters of its flame's light, given out from the wall facing
        // the point (a cylinder seen side on: its intensity the wall's radiance over 2 r h)
        if self.frost > 0.0 {
            for v in &self.votives {
                let Some(f) = self.flames.get(v.flame) else { continue };
                let (dx, dz) = (p.x - v.foot.x, p.z - v.foot.z);
                let out = (dx * dx + dz * dz).sqrt();
                if out < v.radius {
                    continue;
                }
                // the flame's light caught by the wall (about 0.6 of it round a votive taller than its flame),
                // scattered, half of that out of the glass again
                let intensity = VOTIVE_GLOW * self.frost * f.intensity;
                let mid = v.foot + vec3f(dx / out * v.radius, 0.5 * v.tall, dz / out * v.radius);
                let l = mid - p;
                let d2 = l.length_squared().max(1.0e-6);
                let cos = n.dot(l) / d2.sqrt();
                if !(cos > 0.0) {
                    continue;
                }
                let area = 2.0 * v.radius * v.tall;
                e += Vec3f::all(intensity * cos / (d2 + area / std::f32::consts::PI)) * mul(self.glass, Vec3f::all(1.0));
            }
        }
        (e, highlight)
    }

    /// The flames' light on a glass face at `p` (normal `n`, either side), not shadowed: a votive's wall is lit by
    /// its own flame with nothing between.
    fn flame_light_open(&self, p: Vec3f, n: Vec3f) -> Vec3f {
        let mut e = 0.0;
        for f in &self.flames {
            let l = f.centre - p;
            let d2 = l.length_squared().max(1.0e-6);
            e += f.intensity * n.dot(l).abs() / (d2 * d2.sqrt());
        }
        Vec3f::all(e)
    }

    /// The flames seen along the reflected ray from `p` (unit `r`) off a face of roughness `rough`: each widened by
    /// the lobe at its distance.
    fn flames_mirrored(&self, p: Vec3f, r: Vec3f, rough: f32) -> (Vec3f, Vec3f) {
        let ratio = |k: f32| self.ratio(k);
        let (mut rel, mut plain) = (Vec3f::default(), Vec3f::default());
        for f in &self.flames {
            let dist = (f.centre - p).length();
            let blur = (rough * 1.2 * dist).min(dist);
            let (a, b) = f.along(p, r, f32::INFINITY, blur, &ratio);
            if a == Vec3f::default() && b == Vec3f::default() {
                continue;
            }
            // what stands between the face and the flame (a cup's wall, a votive's glass) hides or dims its image
            let pass = self.pass(p, f.centre);
            rel += mul(a, pass);
            plain += mul(b, pass);
        }
        (rel, plain)
    }

    /// Shades the face at `p` (model frame) of normal `n` (toward the ray), material `mat`, triangle `tri`, seen
    /// along `d`; `world` turns a model-frame direction into the map's: (rel, plain, abs).
    fn shade(&self, p: Vec3f, n: Vec3f, d: Vec3f, mat: Mat, tri: usize, light: &CandleLight, world: &dyn Fn(Vec3f) -> Vec3f) -> (Vec3f, Vec3f, Vec3f) {
        let look = match mat {
            Mat::Wax => Look { albedo: self.wax, f0: Vec3f::all(0.035), rough: 0.45 },
            Mat::Pool => Look { albedo: self.wax * 0.7, f0: Vec3f::all(0.04), rough: 0.03 },
            Mat::Wick => Look { albedo: Vec3f::all(0.02), f0: Vec3f::all(0.02), rough: 0.8 },
            Mat::Cup => Look { albedo: Vec3f::all(0.03), f0: vec3f(0.86, 0.87, 0.88), rough: 0.18 },
            Mat::Metal => Look { albedo: self.metal.albedo, f0: self.metal.f0, rough: self.metal.rough },
            Mat::Top => Look { albedo: self.top.albedo, f0: self.top.f0, rough: self.top.rough },
            Mat::Base => Look { albedo: Vec3f::all(0.03), f0: Vec3f::all(0.05), rough: 0.35 },
            Mat::Glass => Look { albedo: Vec3f::default(), f0: Vec3f::all(0.04), rough: 0.0 },
        };
        let inv_pi = 1.0 / std::f32::consts::PI;
        let cos_v = (-d.dot(n)).clamp(0.0, 1.0);
        let spec = fresnel(look.f0, cos_v);
        let r = d - n * (2.0 * d.dot(n));
        // the flames' light, Lambertian, and the scene's; on a rough face (GLOSSY_ROUGH and up) the flames'
        // highlight by GGX, on a smoother one their image mirrored, blurred by its roughness
        let rough_face = look.rough >= GLOSSY_ROUGH;
        let (e, highlight) = self.flame_light_glossy(p, n, rough_face.then_some((d, look.rough)));
        let diffuse = mul(look.albedo, Vec3f::all(1.0) - spec);
        let mut rel = mul(diffuse, e) * inv_pi + mul(spec, highlight);
        let mut plain = Vec3f::default();
        let mut abs = mul(diffuse, light.irradiance(world(n))) * inv_pi;
        if !rough_face {
            let (mr, mp) = self.flames_mirrored(p + n * 1.0e-4, r, look.rough);
            rel += mul(spec, mr);
            plain += mul(spec, mp);
        }
        abs += mul(spec, light.irradiance(world(r))) * inv_pi;
        match mat {
            Mat::Wax => {
                // the wax passes on the light of its top down its wall
                if let Some(&(x, z, top_y, radius)) = self.tops.get(self.owner[tri] as usize) {
                    let depth = (top_y - p.y).max(0.0);
                    let near = self.flames.get(self.owner[tri] as usize).map_or(0.0, |f| f.intensity / ((f.centre - vec3f(x, top_y, z)).length_squared() + radius * radius).max(1.0e-6));
                    let thin = smooth(0.0, 0.5 * radius, radius - ((p.x - x).hypot(p.z - z))).max(0.35);
                    rel += self.wax * (WAX_GLOW * near * (-depth / WAX_REACH).exp() * thin * inv_pi);
                }
            }
            Mat::Wick => {
                // the ember at its tip
                if let Some(f) = self.flames.get(self.owner[tri] as usize) {
                    // the wick's tip (a quarter of the flame up, plus 3 mm: `candle`) glows dull red
                    let up = p.y - (f.foot.y + 0.25 * f.tall + 0.0015);
                    plain += vec3f(0.9, 0.18, 0.03) * (0.06 * smooth(-0.0015, 0.0005, up));
                }
            }
            _ => {}
        }
        (rel, plain, abs)
    }

    /// What the ray from `o` along unit `d` (the model's frame) meets: `texel` its footprint's angle (a flame
    /// narrower than it is widened to it); `world` turns model-frame directions into the map's. None when it meets
    /// nothing, not even a flame's glow.
    pub fn cast(&self, o: Vec3f, d: Vec3f, texel: f32, light: &CandleLight, world: &dyn Fn(Vec3f) -> Vec3f) -> Option<CandleHit> {
        let ratio = |k: f32| self.ratio(k);
        let mut hit_out = CandleHit::default();
        let mut keep = Vec3f::all(1.0);
        let mut at = o;
        let mut walked = 0.0f32;
        let mut solid = false;
        let mut any = false;
        for _ in 0..MAX_CROSSINGS {
            let hit = self.bvh.hit(at + d * PAST, d);
            let t_end = hit.map_or(f32::INFINITY, |h| h.t + PAST);
            // the flames in this stretch of the ray
            for f in &self.flames {
                let dist = (f.centre - o).length();
                let (a, b) = f.along(at, d, t_end, texel * dist * 0.5, &ratio);
                let h = f.halo(at, d, t_end);
                if a != Vec3f::default() || b != Vec3f::default() || h > 0.0 {
                    any = true;
                    hit_out.rel += mul(keep, a + Vec3f::all(h));
                    hit_out.plain += mul(keep, b);
                }
            }
            let Some(h) = hit else { break };
            any = true;
            let p = at + d * t_end;
            walked += t_end;
            let [n0, n1, n2] = self.normals[h.tri];
            let mut n = (n0 * (1.0 - h.u - h.v) + n1 * h.u + n2 * h.v).normalize();
            if !(n.length() > 0.5) {
                n = h.face;
            }
            if n.dot(d) > 0.0 {
                n = -n;
            }
            let mat = self.mats[h.tri];
            if mat == Mat::Glass {
                // its reflection and what lights it, then on through it
                let c = (-d.dot(n)).clamp(0.0, 1.0);
                let f = fresnel(Vec3f::all(0.04), c);
                let r = d - n * (2.0 * d.dot(n));
                // frost etches the face: it blurs the flames' image in the far wall as much as it hazes the glass
                let (mr, mp) = self.flames_mirrored(p + n * 1.0e-4, r, 0.01 + self.frost);
                let world_r = world(r);
                // what the glass scatters of the flame's light (its faces' haze, its frost), more seen edge on
                let haze = (0.012 + 0.25 * self.frost) / c.max(0.12);
                let lit = self.flame_light_open(p, n);
                hit_out.rel += mul(keep, mul(f, mr) + mul(self.glass, lit) * (haze / std::f32::consts::PI));
                hit_out.plain += mul(keep, mul(f, mp));
                hit_out.abs += mul(keep, mul(f, light.irradiance(world_r))) * (1.0 / std::f32::consts::PI);
                keep = mul(keep, self.glass_pass(c));
                at = p;
                continue;
            }
            let (rel, plain, abs) = self.shade(p, n, d, mat, h.tri, light, world);
            hit_out.rel += mul(keep, rel);
            hit_out.plain += mul(keep, plain);
            hit_out.abs += mul(keep, abs);
            solid = true;
            break;
        }
        let _ = walked;
        // a floor candle's pool on the floor round it (outdoors)
        if self.pool && !solid && d.y < -1.0e-6 {
            let t = -o.y / d.y;
            let q = o + d * t;
            if t > 0.0 && q.x.hypot(q.z) < POOL_REACH {
                let e = self.flame_light(q, vec3f(0.0, 1.0, 0.0));
                if e != Vec3f::default() {
                    any = true;
                    hit_out.rel += mul(keep, e) * (POOL_ALBEDO / std::f32::consts::PI);
                }
            }
        }
        if !any {
            return None;
        }
        hit_out.opacity = if solid { 1.0 } else { 1.0 - (keep.x + keep.y + keep.z) / 3.0 };
        Some(hit_out)
    }
}

/// The flat face of a candle light (modifier.rs, a 2D light's or the key's): its flame's teardrop as the camera sees
/// it side on, `u` the face's box units (x across its width, y up its height, each -1..1), at about mean 1 over the
/// disc it fills.
pub fn flame_face(u: Vec2f) -> f32 {
    let t = 0.5 * (u.y + 1.0);
    let r = tear(t);
    if !(r > 1.0e-6) {
        return 0.0;
    }
    let s = u.x.abs() / r;
    if s >= 1.0 {
        return 0.0;
    }
    let (e, _) = emission(t, s, &|_| Vec3f::all(1.0));
    // a chord through the round flame at s: its length over the widest's
    lum(e) * (1.0 - s * s).sqrt() * 2.6
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flame() -> Flame {
        Flame { foot: Vec3f::default(), tall: 0.03, wide: 0.011, lean: Vec3f::default(), bright: 1.0, intensity: 0.0, centre: vec3f(0.0, 0.012, 0.0) }
    }

    /// A flame's mean radiance over its side view is about 1 (the light's own colour, mean luminance).
    #[test]
    fn flame_density_is_unit_mean() {
        let f = flame();
        let ratio = |_k: f32| Vec3f::all(1.0);
        let (mut sum, mut n) = (0.0, 0);
        let steps = 80;
        for i in 0..steps {
            for j in 0..steps {
                let x = (i as f32 + 0.5) / steps as f32 - 0.5;
                let y = (j as f32 + 0.5) / steps as f32;
                let (t, s) = (y, x.abs() * 2.0);
                if s < tear(t) {
                    let o = vec3f(x * f.wide, y * f.tall, 1.0);
                    let (a, _) = f.along(o, vec3f(0.0, 0.0, -1.0), f32::INFINITY, 0.0, &ratio);
                    sum += lum(a);
                    n += 1;
                }
            }
        }
        let mean = sum / n as f32;
        assert!((mean - 1.0).abs() < 0.08, "mean {mean}");
    }
}
