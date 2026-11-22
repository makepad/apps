//! A light's modifier (`LightParams::modifier`): what a real studio light's
//! face looks like up close, generated from the owner's photographed plates
//! (3D Collective, HDR Light Studio, area light maps; measured in
//! local/agent_state/hdri/studio/refs-lamps-measures.md, nothing copied):
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
//! panel; an edge-lit gradient panel; a street lamp's luminaire in its halo
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
//!   gradient's contrast, a street lamp's halo (the night's haze);
//! - `ribs`: an umbrella's or octabox's ribs, a ring's rows of diodes (ten
//!   a row), a fresnel's lens steps (fourteen), a panel's diodes across; a gradient is
//!   linear at 0, radial above; a street lamp has a pole above 0;
//! - `plate`: a beauty dish's centre plate, an umbrella's head (its radius is
//!   half of it), a ring's hole, a bare head's dome, a street lamp's
//!   luminaire, all shares of the radius.
//!
//! Positions are face coordinates (tangent units, x to the viewer's right, y
//! up); `u = q / half` puts the box's edge at 1. Per call: a few exponentials
//! and up to three octaves of value noise, or nine cells of facets, under a
//! microsecond.
use makepad_draw::*;

use super::noise::{fbm2, hash01, hash_u32, value2};
use super::open_box::OpenBox;
use super::params::{ModifierKind, ModifierParams};
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
/// corners on the box fills `OCTAGON_SHARE` of it), a hundredth of that in
/// a faint halo round it.
const TUBE_RADIUS_BOX: f32 = 0.06;
const TUBE_RADIUS_OCTA: f32 = 0.045;
const TUBE_MIN_TAN: f32 = 0.0052;
const TUBE_SHARE_BOX: f32 = 0.77;
const TUBE_SHARE_OCTA: f32 = 0.87;
const OCTAGON_SHARE: f32 = std::f32::consts::FRAC_1_SQRT_2;
/// The area of the tube's crisp disc, 1 - smooth(0.85 R, 1.15 R, r), over pi R².
const TUBE_DISC_AREA: f32 = 1.0045;
/// A fresnel's lens edge and the pitch of the wire guard over it, in box
/// units, and its barrel's level (the area map: 0.006 of the face's mean).
const LENS_R: f32 = 0.69;
const MESH: f32 = 0.15;
const BARREL: f32 = 0.008;
const EDGE: f32 = 0.012;
/// The housing and a rib are not quite black: black fabric and metal catch some light.
const HOUSING: f32 = 0.04;
/// An umbrella's stand: half its width in box units (3-4 % of the radius in the plates).
const STAND_HALF_WIDTH: f32 = 0.017;

#[derive(Clone, Debug)]
pub struct Modifier {
    kind: ModifierKind,
    half: Vec2f,
    rim: f32,
    baffle: f32,
    ribs: f32,
    plate: f32,
    /// What the folds and glints hang on: fixed by the kind and the size, so
    /// a light draws the same every bake and two sizes draw differently.
    seed: u32,
    /// A silver umbrella's hot profile's mean over the disc (`silver_profile`), so the face sits at mean 1.
    norm: f32,
    /// An open box's silver lining (open_box.rs), when `baffle` opens it.
    open: Option<OpenBox>,
    /// A deep silver umbrella's face (silver_deep.rs), when `baffle` is 0.75
    /// or more: the flash's clipped core breaking into folded lobes, with
    /// its own strobe head and stand.
    deep: Option<DeepSilver>,
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).max(0.0).min(1.0);
    t * t * (3.0 - 2.0 * t)
}

fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// exp(-(x / sigma)²): 1 at 0, e^-1 at sigma.
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
    let (mut px, mut py, mut amplitude, mut sum) = (x, y, 0.5f32, 0.0f32);
    for octave in 0..3u32 {
        let n = value2(px, py, seed.wrapping_add(octave.wrapping_mul(0x9e37_79b9)));
        let ridge = 1.0 - (2.0 * n - 1.0).abs();
        sum += ridge * ridge * amplitude;
        amplitude *= 0.5;
        let (rx, ry) = (0.8 * px - 0.6 * py, 0.6 * px + 0.8 * py);
        px = rx * 2.0 + 17.13;
        py = ry * 2.0 + 5.71;
    }
    (sum / 0.875).min(1.0)
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
/// the nearest point and the second, and the nearest's own random shade.
fn cells(x: f32, y: f32, seed: u32) -> (f32, f32, f32) {
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

/// The crease where two cells meet, a line `width` of a cell wide: 1 on
/// it, 0 off it, `crease_mean(width)` of the plane.
fn crease(d1: f32, d2: f32, width: f32) -> f32 {
    1.0 - smooth(0.0, width, d2.sqrt() - d1.sqrt())
}

/// The share of the plane `crease` covers at `width` (one uniform point
/// per cell; measured: 1.29 x the width within 1 % up to 0.05, 0.124 at
/// 0.1).
fn crease_mean(width: f32) -> f32 {
    1.29 * width
}


/// A facet's brightness from its random tilt `u` (0..1): log-uniform over
/// a factor e^k, as a mirror's image of the flash goes with the facet's
/// angle. Mean 1 (the mean of e^(k (u - 1/2)) is 2 sinh(k / 2) / k).
fn log_uniform(u: f32, k: f32) -> f32 {
    (k * (u - 0.5)).exp() * k / (2.0 * (0.5 * k).sinh())
}

/// Crumpled silver foil as the open boxes' plates read it texel by texel:
/// broad soft folds (a cloud of bright and dark patches with the creases'
/// ridges over it) and small flat facets of log-uniform brightness (the
/// flash mirrored by facets at every tilt), spread so that a tenth of the texels
/// sit under 0.58 of the median and a tenth above 1.9 at `contrast` 0.85
/// (a softbox's walls; an octabox's segments, 0.95, spread to 0.48 and
/// 2.2), the median 0.8-0.88 of the mean; and the sparkle, the flash's
/// glints in the creases of a net two hundred cells across the face, finer
/// than a bake's texel, so a texel in forty (`sparkle` 0.02 of a cell; an
/// octabox's wider, 0.03, one in 26) catches one at a few times the mean.
/// The glints hold `glints` of the light (0.06 in a softbox, 0.11 in an
/// octabox). The folds take their own
/// coordinates `(fx, fy)`: stretched along a wall, they run along it, as a
/// box's do. Mean 1.
fn foil(x: f32, y: f32, (fx, fy): (f32, f32), contrast: f32, glints: f32, sparkle: f32, seed: u32) -> f32 {
    let folds = cloud(fx * 2.5, fy * 2.5, contrast.min(1.0), seed ^ 0x2b7) * creased(fx * 3.5, fy * 3.5, 1.4 * contrast, seed ^ 0x79);
    let (_, _, fine) = cells(x * 40.0 + 1.3, y * 40.0 + 9.2, seed ^ 0x4c9);
    let base = folds * log_uniform(fine, contrast);
    let (d1, d2, _) = cells(x * 100.0, y * 100.0, seed ^ 0x5d1);
    (1.0 - glints) * base + glints * crease(d1, d2, sparkle) / crease_mean(sparkle)
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
fn rounded_box(p: Vec2f, half: Vec2f, corner: f32) -> f32 {
    let q = vec2f(p.x.abs() - half.x + corner, p.y.abs() - half.y + corner);
    vec2f(q.x.max(0.0), q.y.max(0.0)).length() + q.x.max(q.y).min(0.0) - corner
}

/// 1 inside a signed distance's shape, 0 outside, over the soft edge.
fn inside(sdf: f32) -> f32 {
    1.0 - smooth(-EDGE, 0.0, sdf)
}

/// The flash through two layers of cloth over the box distance `depth` (0
/// at the middle, 1 at the box's edge), as the plate reads it over its
/// mean: 2.6 at the middle, 1.3 half way, 0.55 at 0.85, falling under the
/// frame to 0.13 at the edge. Mean 1 over the box.
fn cloth_falloff(depth: f32) -> f32 {
    (0.1 + 2.5 * (-2.4 * depth * depth).exp()) * (1.0 - 0.8 * smooth(0.88, 1.0, depth))
}

/// A hotspot over a disc, mean 1 over it: `floor` plus a gaussian of rate
/// `k` (0 is flat), divided by their mean over the unit disc,
/// floor + (1 - floor) (1 - e^-k) / k.
fn hotspot(r: f32, k: f32, floor: f32) -> f32 {
    let mean = floor + (1.0 - floor) * if k > 1.0e-4 { (1.0 - (-k).exp()) / k } else { 1.0 };
    (floor + (1.0 - floor) * (-k * r * r).exp()) / mean
}

/// The strobe head and its stand in silhouette in front of an umbrella: the
/// reflector dish, a disc of radius `head` at the middle; the head's body
/// behind it, a rounded block showing under the dish; the yoke and the
/// stand's spigot, narrow; and the stand's bar straight down past the edge. 1 where nothing hides the face, and without
/// a head (under 0.01) nothing does.
fn head_and_stand(u: Vec2f, head: f32) -> f32 {
    if head < 0.01 {
        return 1.0;
    }
    let dish = 1.0 - smooth(head, head + EDGE, u.length());
    let body = inside(rounded_box(vec2f(u.x, u.y + 0.95 * head), vec2f(0.85 * head, 0.55 * head), 0.35 * head));
    let yoke = inside(rounded_box(vec2f(u.x, u.y + 1.75 * head), vec2f(0.32 * head, 0.45 * head), 0.1 * head));
    let spigot = inside(rounded_box(vec2f(u.x, u.y + 2.5 * head), vec2f(0.6 * STAND_HALF_WIDTH / 0.017 * 0.03, 0.4 * head), 0.02));
    let stand = if u.y < -2.2 * head { 1.0 - smooth(STAND_HALF_WIDTH, STAND_HALF_WIDTH + EDGE, u.x.abs()) } else { 0.0 };
    let clamp = yoke.max(spigot);
    1.0 - 0.985 * dish.max(body).max(clamp).max(stand)
}

/// The ribs of an umbrella as thin lines from the head outward: dark at a
/// positive `depth` (a rib's shadow on a silver bowl), light at a negative
/// one (a white rib seen through white cloth).
fn rib_lines(u: Vec2f, n: f32, depth: f32, from_r: f32) -> f32 {
    1.0 - depth * line(spoke_gap(u, n, 0.0), LINE) * smooth(from_r, from_r + 0.08, u.length())
}

impl Modifier {
    /// The modifier of `p` (as clamp() holds it) on a face whose tangent half
    /// extents are `half`. None for "none" and for a window (a gobo, not a
    /// pattern on the face).
    pub fn new(p: &ModifierParams, half: Vec2f) -> Option<Modifier> {
        let p = p.clamped();
        let kind = p.kind();
        if matches!(kind, ModifierKind::None | ModifierKind::Window) {
            return None;
        }
        let half = vec2f(half.x.max(1.0e-6), half.y.max(1.0e-6));
        let seed = hash_u32(half.x.to_bits() ^ hash_u32(half.y.to_bits()).rotate_left(7) ^ (kind as u32).wrapping_mul(0x9e37_79b9));
        let norm = match kind {
            ModifierKind::SilverUmbrella => {
                let t = ((p.baffle - 0.3) / 0.7).max(0.0).min(1.0);
                // The profile's mean over the unit disc: the midpoint sum of profile(r) 2 r dr.
                let n = 256;
                (0..n).map(|i| (i as f32 + 0.5) / n as f32).map(|r| Self::silver_profile(r, t) * 2.0 * r / n as f32).sum::<f32>().max(1.0e-3)
            }
            _ => 1.0,
        };
        let short = half.x.min(half.y);
        let open = match kind {
            ModifierKind::Softbox | ModifierKind::Strip if p.baffle > 0.75 => Some(OpenBox::new(false, vec2f(half.x / short, half.y / short), TUBE_SHARE_BOX, seed)),
            ModifierKind::Octabox if p.baffle > 0.75 => Some(OpenBox::new(true, vec2f(1.0, 1.0), TUBE_SHARE_OCTA, seed)),
            _ => None,
        };
        let deep = match kind {
            ModifierKind::SilverUmbrella if p.baffle >= 0.75 => Some(DeepSilver::new(if p.ribs >= 3 { p.ribs } else { 16 }, seed)),
            _ => None,
        };
        Some(Modifier { kind, half, rim: p.rim, baffle: p.baffle, ribs: p.ribs as f32, plate: p.plate, seed, norm, open, deep })
    }

    /// The face's emission at `q` as a share of the light's: about 1 over the
    /// face, 0.04 in the housing, far above at a hotspot. 1 where `q` is not
    /// a point (a NaN direction leaves the light as it is).
    pub fn factor(&self, q: Vec2f) -> f32 {
        if !q.x.is_finite() || !q.y.is_finite() {
            return 1.0;
        }
        let u = vec2f(q.x / self.half.x, q.y / self.half.y);
        // How far in from the box's edge, 0 at it and 1 at the middle: a box's
        // (softbox, strip, panel) or a disc's (the round ones). The rim is that
        // share of the smaller half size.
        let boxed = 1.0 - u.x.abs().max(u.y.abs());
        let r = u.length();
        let round = 1.0 - r;
        let in_box = |inset: f32| smooth(inset, inset + EDGE, boxed);
        let in_round = |inset: f32| smooth(inset, inset + EDGE, round);
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
                ModifierKind::Softbox | ModifierKind::Strip => housing(inside(cloth_sdf(u, self.aspect(), self.rim, 0.015, 0.06))) * self.softbox(u, r, fx, fy, boxed),
                ModifierKind::Octabox => self.octabox(u, r, fx, fy),
                ModifierKind::Umbrella => self.umbrella(u, r),
                ModifierKind::SilverUmbrella => match &self.deep {
                    Some(deep) => deep.factor(u),
                    None => self.silver_umbrella(u, r),
                },
                ModifierKind::BeautyDish => housing(in_round(self.rim)) * self.beauty_dish(u, r),
                ModifierKind::RingLight => housing(in_round(self.rim)) * self.ring_light(u, r),
                ModifierKind::Fresnel => self.fresnel(u, r),
                ModifierKind::Bulb => self.bulb(u, r),
                ModifierKind::LedPanel => housing(in_box(self.rim)) * self.led_panel(u, d),
                ModifierKind::Gradient => housing(in_box(self.rim)) * self.gradient(u, r, fx, fy),
                ModifierKind::StreetLamp => self.street_lamp(u, r),
                ModifierKind::None | ModifierKind::Window => 1.0,
            };
        if f.is_finite() {
            f.max(0.0)
        } else {
            1.0
        }
    }

    /// The bare tube at the middle of an open box, which the plates put at
    /// 0.77 to 0.87 of the face's light: a crisp disc (round in angle: `d`
    /// is the real-unit radius) with a faint halo, 1 / (1 + (r / r0)²)^1.5,
    /// round it, at the level that gives it `core_mass`, so about 250 x the
    /// face's mean at the middle of a 60 x 90 box. 0 without a core.
    fn tube(&self, d: f32) -> f32 {
        let mass = self.core_mass();
        if mass <= 0.0 {
            return 0.0;
        }
        let (share, glow, reach) = if self.kind == ModifierKind::Octabox { (TUBE_RADIUS_OCTA, 0.01, 1.8) } else { (TUBE_RADIUS_BOX, 0.01, 1.5) };
        let radius = (share * self.half.x.min(self.half.y)).max(TUBE_MIN_TAN);
        let dq = d * self.half.x.max(self.half.y);
        let level = (1.0 - glow) * mass / (TUBE_DISC_AREA * std::f32::consts::PI * radius * radius);
        // The halo's integral over the plane is 2 pi r0².
        let r0 = reach * radius;
        let halo = glow * mass / (std::f32::consts::TAU * r0 * r0);
        level * (1.0 - smooth(0.85 * radius, 1.15 * radius, dq)) + halo * (1.0 + (dq / r0).powi(2)).powf(-1.5)
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
    fn softbox(&self, u: Vec2f, r: f32, fx: f32, fy: f32, boxed: f32) -> f32 {
        let open = smooth(0.75, 1.0, self.baffle);
        let inner = smooth(0.0, 0.7, self.baffle) * (1.0 - open);
        let depth = 1.0 - boxed;
        let falloff = cloth_falloff(depth);
        let cloth = creased(fx * 3.0, fy * 3.0, 0.05 + 0.3 * inner, self.seed) * cloud(fx * 1.5, fy * 1.5, 0.03, self.seed ^ 0x11);
        let seam = 1.0 - 0.05 * line(u.x, 0.01).max(line(u.y, 0.01));
        let closed = falloff * cloth * seam;
        // Which side of the box a point is on, and the folds of that side's
        // foil running along it (toward the opening on the sides, up the
        // top and bottom).
        let on_sides = u.x.abs() > u.y.abs();
        let side = if on_sides { i32::from(u.x < 0.0) } else { 2 + i32::from(u.y < 0.0) };
        let seed = self.seed ^ (side as u32).wrapping_mul(0x9e37_79b9);
        let shade = 0.8 + 0.4 * hash01(side, 0, 0, self.seed ^ 0x5e);
        let diagonal = ((u.x.abs() - u.y.abs()) * std::f32::consts::FRAC_1_SQRT_2).abs();
        let mut diffused = closed;
        if inner > 0.0 {
            // The inner cloth: inset 0.2 of the short half size from the box,
            // rounded, sagging; the walls' band round it, its folds running
            // along the band; the cloth's shadow line on the walls.
            let aspect = self.aspect();
            let inner_sdf = cloth_sdf(u, aspect, 0.2, 0.015, 0.08);
            let inner_face = (0.55 * falloff + 14.0 * gauss(r, 0.09) + 3.0 * gauss(r, 0.28)) * cloth * seam;
            let band_folds = if on_sides { (fx * 1.6, fy * 0.45) } else { (fx * 0.45, fy * 1.6) };
            let walls_level = 0.6 * (1.0 - 0.5 * smooth(0.84, 0.92, depth)) + 0.3 * gauss(depth - 0.8, 0.06);
            let shadow = 1.0 - 0.5 * line(inner_sdf - 0.025, 0.03);
            let walls = walls_level * shade * shadow * Self::silver_walls(fx, fy, band_folds, (0.85, 0.06, 0.02), diagonal, r, seed);
            diffused = mix(closed, mix(walls, inner_face, inside(inner_sdf)), inner);
        }
        if open <= 0.0 {
            return diffused;
        }
        // The open box: the silver lining as the tube lights it.
        let walls = self.open.as_ref().map_or(0.0, |lining| lining.walls(u));
        mix(diffused, walls, open)
    }

    /// The box's half sizes in units of the shorter one (1 on the short side).
    fn aspect(&self) -> Vec2f {
        let short = self.half.x.min(self.half.y);
        vec2f(self.half.x / short, self.half.y / short)
    }

    /// A side of an open box: crumpled silver foil (`foil`, as `(contrast,
    /// glints, sparkle)` set it, its folds on `folds`), a dark seam with a lit edge along `seam`
    /// (the distance to where two sides meet; the edge fades toward the
    /// head). Mean about 1.
    fn silver_walls(fx: f32, fy: f32, folds: (f32, f32), (contrast, glints, sparkle): (f32, f32, f32), seam: f32, r: f32, seed: u32) -> f32 {
        let seams = (1.0 - 0.5 * line(seam, 0.012)) * (1.0 + 0.3 * line(seam, 0.09) * smooth(0.12, 0.3, r));
        foil(fx, fy, folds, contrast, glints, sparkle, seed) * seams
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
        let closed = (0.45 + 1.8 * (-3.5 * r * r).exp()) * (1.0 - 0.4 * smooth(0.86, 1.0, depth));
        let core = 14.0 * gauss(r, 0.09) + 3.0 * gauss(r, 0.28);
        let n = if self.ribs >= 3.0 { self.ribs } else { 8.0 };
        let seams = 1.0 - (0.03 + 0.25 * inner) * line(spoke_gap(u, n, 0.0), 0.7 * LINE);
        let cloth = creased(fx * 3.0, fy * 3.0, 0.04 + 0.3 * inner, self.seed) * cloud(fx * 1.5, fy * 1.5, 0.03, self.seed ^ 0x11);
        // Each segment its own cloth (its folds offset per segment).
        let segment = ((a / std::f32::consts::TAU * n).rem_euclid(n)).floor() as i32;
        let seg_folds = (fx * 0.7 + 1.7 * segment as f32, fy * 0.7);
        let seed = self.seed ^ (segment as u32).wrapping_mul(0x9e37_79b9);
        let shade = 0.8 + 0.4 * hash01(segment, 0, 0, self.seed ^ 0x5e);
        let mut diffused = closed * cloth * seams;
        if inner > 0.0 {
            let inner_edge = smooth(0.26, 0.26 + EDGE, octagon - 0.3 * sag);
            let inner_face = (0.55 * closed + core) * cloth * seams;
            let walls_level = 0.6 * (1.0 - 0.5 * smooth(0.84, 0.92, depth)) + 0.3 * gauss(depth - 0.8, 0.06);
            let shadow = 1.0 - 0.5 * line(octagon - 0.26 + 0.025, 0.03);
            let walls = walls_level * shade * shadow * Self::silver_walls(fx, fy, seg_folds, (0.85, 0.06, 0.02), spoke_gap(u, n, 0.0), r, seed);
            diffused = mix(diffused, mix(walls, inner_face, inner_edge), inner);
        }
        if open <= 0.0 {
            return inside * diffused;
        }
        // Open: the silver segments as the tube lights them.
        let walls = self.open.as_ref().map_or(0.0, |lining| lining.walls(u));
        inside * mix(diffused, walls, open)
    }

    /// The edge of an umbrella: the cloth runs straight from rib to rib (the
    /// `n`-gon with its corners at the ribs, taut) and sags a little between
    /// them; past it there is nothing (0).
    fn scalloped(&self, u: Vec2f, n: f32, r: f32) -> f32 {
        let sector = std::f32::consts::TAU / n;
        let a = u.x.atan2(u.y);
        let off = (a / sector - (a / sector).round()) * sector;
        let polygon = (0.5 * sector).cos() / off.cos().max(1.0e-3);
        let sag = (1.0 - polygon) + 0.012 * (0.5 * n * a).sin().powi(2);
        let edge = 1.0 - self.rim - sag;
        1.0 - smooth(edge - EDGE, edge, r)
    }

    /// A white umbrella: the strobe head and its stand in silhouette, the
    /// face brightest in a ring round the head and falling to the edge
    /// (`baffle` sets how fast: 0 the flat parabolic, 0.6 the white bounce),
    /// the ribs as light lines through the cloth, the panels between them a
    /// little brighter down their middles, folded.
    fn umbrella(&self, u: Vec2f, r: f32) -> f32 {
        let n = if self.ribs >= 3.0 { self.ribs } else { 8.0 };
        let a = u.x.atan2(u.y);
        let hot = hotspot(r, 5.3 * self.baffle, 0.08);
        let head = 0.5 * self.plate;
        let silhouette = head_and_stand(u, head);
        let ribs = rib_lines(u, n, -0.12 - 0.25 * self.baffle, head + 0.03);
        // The cloth between the ribs: a little brighter down each panel's middle, folded.
        let panels = 1.0 + 0.05 * self.baffle * (0.5 * n * a).sin().powi(2) * 2.0 - 0.05 * self.baffle;
        let folds = creased(a * n * 0.48, r * 4.0, 0.3, self.seed) * cloud(u.x * 2.5, u.y * 2.5, 0.08, self.seed ^ 0x21);
        self.scalloped(u, n, r) * hot * silhouette * ribs * panels * folds
    }

    /// A silver bowl's hot zone over the radius, in its plate's median units:
    /// a shallow bowl (`t` 0) a bump peaking 3 x in a ring round the head
    /// and fading to 0.3 at the edge; a deep one (`t` 1) the flash's mirror
    /// image, a plateau 70 x out to 0.4 of the radius, a cliff to 0.55, and a
    /// flat outer zone at 0.8 (the EV ladder's umbrella: its blown disc holds
    /// the plateau and grows past the cliff only stops later). Mixed by `t`.
    fn silver_profile(r: f32, t: f32) -> f32 {
        Self::silver_profile_at(r, t, 0.0)
    }

    /// `silver_profile` with the deep bowl's cliff moved out by `reach` (box
    /// units): the blown zone runs further down each panel's middle than at
    /// the ribs, a star of as many long tapered points as ribs, its edge
    /// ragged. The mean (`norm`) is taken at reach 0, which the star's
    /// points and dents balance.
    fn silver_profile_at(r: f32, t: f32, reach: f32) -> f32 {
        let shallow = 0.3 + 2.9 * (-((r - 0.3).max(0.0) / 0.33).powi(2)).exp();
        let deep = 0.8 + 69.0 * (1.0 - smooth(0.33 + reach, 0.55 + reach, r));
        mix(shallow, deep, t)
    }

    /// A silver umbrella: the flash mirrored by the bowl (`silver_profile`,
    /// at mean 1; `baffle` 0.3 a shallow bowl, 1 a deep one), streaked along
    /// the panels of a deep bowl (bright down each panel's middle, dark at
    /// the ribs, fading with the hot zone), crumpled and glinting, the head
    /// and its stand in front.
    fn silver_umbrella(&self, u: Vec2f, r: f32) -> f32 {
        let n = if self.ribs >= 3.0 { self.ribs } else { 16.0 };
        let a = u.x.atan2(u.y);
        let t = ((self.baffle - 0.3) / 0.7).max(0.0).min(1.0);
        let panel = (0.5 * n * a).sin().powi(2);
        // The blown zone's edge: 0.1 further out down a panel's middle than
        // at a rib (a star of long tapered points), and ragged.
        let ragged = 0.03 * t * (2.0 * fbm2(a * 3.0, r * 5.0, 2, self.seed ^ 0x9a) - 1.0);
        let hot = Self::silver_profile_at(r, t, 0.05 * t * (panel * panel - 0.375) + ragged) / self.norm;
        // Past the blown disc the flash's streaks run on down each panel's
        // middle, fading over the outer zone (a deep bowl's; mean about 0.4
        // over the outer ring, taken from the outer zone's own level).
        let spill = t * t * 9.0 * panel.powi(6) * (-(r - 0.5).max(0.0) / 0.16).exp() * smooth(0.4, 0.5, r) / self.norm;
        let along = fbm2(r * 6.0, (a / std::f32::consts::TAU * n).floor() * 3.7 + 0.5, 2, self.seed ^ 0x44);
        // Mean 1 over the ring of panels: (0.05 + 0.95 x 3/8) x (0.6 + 0.8 x 0.5).
        let streaked = (0.05 + 0.95 * panel * panel * panel * panel) * (0.6 + 0.8 * along) / 0.406;
        // The streaks show where the blown disc ends: in it everything is white.
        let streaks = mix(1.0, streaked, t * smooth(0.3, 0.45, r) * (1.0 - smooth(0.6, 0.9, r)));
        // The foil: fine crumple, each panel lighter down its middle and its own shade.
        let segment = ((a / std::f32::consts::TAU * n).rem_euclid(n)).floor() as i32;
        let panel_shade = (0.75 + 0.5 * panel) * (0.85 + 0.3 * hash01(segment, 0, 0, self.seed ^ 0x5e)) / 1.0;
        let crumple = foil(u.x * 0.5, u.y * 0.5, (u.x * 0.6, u.y * 0.6), 0.25, 0.04, 0.02, self.seed) * panel_shade;
        let head = 0.5 * self.plate;
        let silhouette = head_and_stand(u, head);
        let ribs = rib_lines(u, n, 0.3 + 0.5 * t, head + 0.02);
        self.scalloped(u, n, r) * (hot * streaks + spill) * crumple * silhouette * ribs
    }

    /// A beauty dish: the dark centre plate on its three spokes, the bowl
    /// brightest in a ring just past it, dimmer half way out and brighter
    /// again toward its bright rolled lip, hammered; with its sock (`baffle`)
    /// an even disc, softly darker in the middle.
    fn beauty_dish(&self, u: Vec2f, r: f32) -> f32 {
        let plate_r = self.plate;
        let plate = 1.0 - 0.992 * (1.0 - smooth(plate_r, plate_r + EDGE, r));
        let bowl = 0.8 + 0.5 * gauss(r - plate_r - 0.06, 0.07) + 0.3 * smooth(0.65, 0.9, r);
        let lip = 1.0 + 2.5 * gauss(r - 0.94, 0.015);
        let spokes = 1.0 - 0.5 * line(spoke_gap(u, 3.0, 0.3), 0.01) * smooth(plate_r + 0.1, plate_r, r);
        let hammer = cloud(u.x * 12.0, u.y * 12.0, 0.08, self.seed);
        let bare = plate * bowl * lip * spokes * hammer;
        let sock = (0.52 + 0.53 * smooth(0.2, 0.55, r) - 0.5 * smooth(0.85, 1.0, r)) * cloud(u.x * 4.0, u.y * 4.0, 0.015, self.seed ^ 0x31);
        mix(bare, sock, self.baffle)
    }

    /// An LED ring light: rows of diodes (`ribs` / 10 of them) on a white ring
    /// round a hole of `plate` of the radius, staggered row to row, each a
    /// sharp disc 15 x the panel between; `baffle` puts a diffuser over them,
    /// which leaves them showing softly through.
    fn ring_light(&self, u: Vec2f, r: f32) -> f32 {
        let hole = self.plate.min(0.9);
        let outer = 1.0 - self.rim;
        if r < hole || outer <= hole + 0.02 {
            return HOUSING;
        }
        let rows = (self.ribs / 10.0).round().max(1.0).min(8.0);
        let pitch = (outer - hole) / rows;
        let k = ((r - hole) / pitch).floor().min(rows - 1.0);
        let rk = hole + (k + 0.5) * pitch;
        let nk = (std::f32::consts::TAU * rk / pitch).round().max(6.0);
        let stagger = if (k as i32) % 2 == 1 { 0.5 } else { 0.0 };
        let a = u.x.atan2(u.y);
        let aj = ((a / std::f32::consts::TAU * nk - stagger).round() + stagger) * std::f32::consts::TAU / nk;
        let centre = vec2f(rk * aj.sin(), rk * aj.cos());
        let spread = 1.0 + 0.6 * self.baffle;
        let dot = 1.0 - smooth(0.26 * pitch * spread, 0.36 * pitch * spread, (u - centre).length());
        let gap = 0.06 + 0.4 * self.baffle;
        let level = gap + (1.0 - gap) * dot;
        // The mean over the ring: the gap plus each dot's area in its pitch² cell.
        let mean = gap + (1.0 - gap) * (std::f32::consts::PI * (0.31 * spread).powi(2)).min(1.0);
        level / mean * cloud(u.x * 5.0, u.y * 5.0, 0.03, self.seed)
    }

    /// A fresnel spot as the area map reads it, over the face's mean: the
    /// lamp's image through the lens a disc 14 x with a hot centre (19 x
    /// within 0.05 of the radius, never under 0.3 degrees), a cliff at
    /// 0.15-0.21, then the lens
    /// zone falling to the lens edge at 0.69 (6 x at 0.19, 3.7 at 0.24, 1
    /// at 0.33, 0.27 at 0.48, 0.03 at the edge: fast first, then two thirds
    /// a ring, faster again at the edge) in `ribs` steps: each ring of the
    /// lens one brightness with a sharp dark edge, the middle ring a disc
    /// half a pitch across; a wire guard over the lens; the barrel past it
    /// near black (0.008) with its thin metal rim 0.03 inside `rim`, and
    /// nothing but that past the lens, the plate's black.
    /// `baffle` focuses the image (1 spot, 0 flood: the image spread two and
    /// a half times, cut by the lens). The steps, the guard and the glass
    /// are finer than a small spot's bake resolves (a 7 degree key in a 1K
    /// map is 20 texels across, and point-sampled rings put its key off by
    /// up to 16 %), so they fade out under about 14 degrees and are full
    /// from 24.
    fn fresnel(&self, u: Vec2f, r: f32) -> f32 {
        let s = 1.0 + 1.5 * (1.0 - self.baffle);
        // The image's finest feature (its hot centre, its cliff's edge) is
        // never under 0.3 degrees: a 7 degree key's half-texel centre at 1K
        // put its key 1.3 % off the map.
        let soft = (TUBE_MIN_TAN / self.half.x.min(self.half.y)).max(0.05 * s);
        let image = |r: f32| {
            let disc = 14.0 + 5.0 * gauss(r, soft);
            let past = (r - 0.238 * s).max(0.0);
            let tail = 1.5 * (-past / (0.14 * s)).exp() + 2.2 * (-past / (0.04 * s)).exp();
            mix(disc, tail, smooth(0.18 * s - 0.6 * soft, 0.18 * s + 0.6 * soft, r)) * (1.0 - 0.6 * smooth(0.58, LENS_R, r)) / (s * s)
        };
        let fine = smooth(0.12, 0.21, self.half.x.min(self.half.y));
        // The lens steps: `ribs` rings over the lens past the middle disc,
        // bounded at the half pitches, each at the image's level at its
        // middle, a faint dark line where two meet.
        let pitch = LENS_R / (self.ribs.max(3.0) + 0.5);
        let stepped = image((r / pitch).round() * pitch);
        let lamp = mix(image(r), stepped, 0.7 * fine);
        let to_step = (0.5 - ((r / pitch - 0.5).rem_euclid(1.0) - 0.5).abs()) * pitch;
        let steps = 1.0 - 0.45 * fine * line(to_step, 0.006);
        let to_wire = |x: f32| 0.5 * MESH - (x.rem_euclid(MESH) - 0.5 * MESH).abs();
        let guard = 1.0 - 0.3 * fine * line(to_wire(u.x), 0.012).max(line(to_wire(u.y), 0.012));
        let glass = cloud(u.x * 10.0, u.y * 10.0, 0.1 * fine, self.seed);
        let lens = 1.0 - smooth(LENS_R - 0.01, LENS_R + 0.01, r);
        let barrel = BARREL * (1.0 - lens);
        let metal = 0.1 * gauss(r - (1.0 - self.rim - 0.03), 0.012);
        lamp * steps * guard * glass * lens + barrel + metal
    }

    /// A bare head: the frosted dome, `plate` of the radius, with the flash
    /// tube's ring and the modelling lamp's dark cap in it (`baffle` frosts
    /// them away), at a level that puts the box's mean near 1; the black
    /// housing round it catches a little light at its rim.
    fn bulb(&self, u: Vec2f, r: f32) -> f32 {
        let dome_r = self.plate.max(0.15);
        let inside = 1.0 - smooth(dome_r - 0.02, dome_r + 0.02, r);
        // The dome's core, 25 x the frosted collar round it (the plate: 500 x
        // the median inside 0.25, 20 x from 0.38 to 0.5); the collar's mean
        // share over the dome is about a third.
        let core = 0.04 + 0.96 * (1.0 - smooth(0.52 * dome_r, 0.62 * dome_r, r));
        let tube = 1.0 + 1.2 * gauss(r - 0.7 * dome_r, 0.08 * dome_r) * (1.0 - self.baffle);
        let cap = 1.0 - 0.6 * (1.0 - smooth(0.25 * dome_r, 0.35 * dome_r, r)) * (1.0 - self.baffle);
        let frost = cloud(u.x * 6.0, u.y * 6.0, 0.1, self.seed);
        let dome = (0.9 / (0.33 * dome_r * dome_r)) * inside * core * tube * cap * frost;
        let housing = 0.004 * (1.0 - inside) * (1.0 + 4.0 * gauss(r - dome_r - 0.12, 0.03));
        dome + housing
    }

    /// An LED panel: `ribs` diodes across, as many rows as the box's aspect
    /// gives, each a sharp disc a thousand times the black panel between (a faint dark dot
    /// at each cell's corners), the whole at mean 1; `baffle` diffuses them
    /// into soft discs over a bright panel and brings in the diffused panel's
    /// soft hotspot.
    fn led_panel(&self, u: Vec2f, d: f32) -> f32 {
        let n = self.ribs.max(4.0);
        let aspect = self.half.y / self.half.x;
        let (nx, ny) = (n, (n * aspect).round().max(1.0));
        let (cx, cy) = ((u.x * 0.5 + 0.5) * nx, (u.y * 0.5 + 0.5) * ny);
        let f = vec2f(cx - cx.floor() - 0.5, cy - cy.floor() - 0.5);
        let spread = 1.0 + 1.0 * self.baffle;
        let dot = 1.0 - smooth(0.2 * spread, 0.28 * spread, f.length());
        let gap = 0.001 + 0.9 * self.baffle;
        let corner = ((0.5 - f.x.abs()).powi(2) + (0.5 - f.y.abs()).powi(2)).sqrt();
        let corner_dot = 1.0 - 0.4 * gauss(corner, 0.09) * (1.0 - self.baffle);
        let lattice = gap + (1.0 - gap) * dot;
        let mean = gap + (1.0 - gap) * (0.18 * spread * spread).min(1.0);
        let soft = mix(1.0, hotspot(d, 3.5, 0.2), self.baffle);
        lattice / mean * corner_dot * soft
    }

    /// An edge-lit gradient panel: `ribs` 0 a log-linear ramp from the
    /// viewer's left edge to the right, 1 + 24 x `baffle` from end to end
    /// (the light's roll turns it); above 0 a soft radial spot. Mean 1.
    fn gradient(&self, u: Vec2f, r: f32, fx: f32, fy: f32) -> f32 {
        let veil = cloud(fx * 2.0, fy * 2.0, 0.01, self.seed);
        if self.ribs < 1.0 {
            let ratio = 1.0 + 24.0 * self.baffle;
            let t = (u.x + 1.0) * 0.5;
            let mean = if ratio > 1.0001 { (1.0 - 1.0 / ratio) / ratio.ln() } else { 1.0 };
            ratio.powf(-t) / mean * veil
        } else {
            hotspot(r, 2.0 + 6.0 * self.baffle, 0.1) * veil
        }
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

/// The panel's names for `MODIFIER_KIND_NAMES`, in its order.
pub const MODIFIER_KIND_LABELS: &[&str] = &[
    "Plain", "Softbox", "Strip box", "Octabox", "Umbrella", "Silver umbrella", "Beauty dish", "Ring light", "Fresnel spot", "Bare bulb", "LED panel", "Gradient", "Street lamp", "Window",
];

/// Makes `light` a light of `kind` as a studio has it, where it is: its
/// shape, its size as seen from a few metres off, its edge, its colour and
/// its modifier. Its place, roll, name, brightness and key are kept. "Plain"
/// takes the modifier (and a window's glass) off and keeps the rest.
pub fn apply_template(light: &mut super::params::LightParams, kind: ModifierKind) {
    use super::params::{GoboParams, PolygonParams};
    let modifier = |kind: &str, rim: f32, baffle: f32, ribs: u32, plate: f32| Some(ModifierParams { kind: kind.to_string(), rim, baffle, ribs, plate });
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
