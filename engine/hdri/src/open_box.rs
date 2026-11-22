//! An open softbox's or octabox's silver lining, drawn as the real one is
//! lit: the box is a pyramid (four walls, or eight segments for an octabox)
//! from the speed ring at the back to the opening, its fabric folded where
//! the rods pull and the ring gathers it, and each point of a wall is shaded
//! by the bare tube at the back as a crumpled silver surface would be.
//!
//! The folds are a height field per wall, placed by the fabric's mechanics
//! (the owner's recipe for procedural wrinkles): a tension field from each
//! rod tip at the opening toward the ring, noise sampled in each fan's own
//! frame (stretched along the tension, squeezed across it, domain-warped so
//! the folds wiggle and branch), a crease profile between pinched and soft;
//! larger folds across the wall's middle; an octabox's large folds gathered
//! round its ring; a fine crinkle in patches. The surface normal follows the
//! height's slope, and the tube lights it: diffuse and two glossy lobes,
//! the tube shining mostly forward but also sideways onto the walls.
//!
//! The brightness from the ring to the opening then follows the plate's
//! measured profile (local/agent_state/hdri/studio/refs-lamps-measures.md,
//! the photographed open boxes RLS012 and RLS019, smoothed), so the walls
//! sit where the real ones do while the lighting keeps the folds' detail;
//! the softbox is darkened toward the lamp after that. The level tables are
//! sampled once per light (`OpenBox::new`), not per texel.
//!
//! Per texel: three height evaluations (about thirty value-noise lookups
//! each) and the lighting, a few microseconds; only open boxes pay it.
use makepad_draw::*;

use super::noise::{fbm2, hash_u32, value2};

/// How one kind of open box is drawn. Lengths are in the box's own units:
/// the shorter half size of the opening is 1.
#[derive(Clone, Copy, Debug)]
struct Look {
    /// The box's depth, and the back's half size as a share of the opening's.
    depth: f32,
    back: f32,
    /// The tube's height in the box (a share of the depth from the opening),
    /// the viewer's distance in front of the opening (negative), and the
    /// tube's emission: forward as cos^k_emit, `emit_floor` of it sideways.
    lamp_z: f32,
    view_z: f32,
    k_emit: f32,
    emit_floor: f32,
    /// The silver's diffuse share and its glossy lobes (share, sharpness).
    kd: f32,
    lobes: [(f32, f32); 2],
    /// The tension folds from the rod tips: along and across frequencies,
    /// domain warp, pinched share of the crease profile, reach along the
    /// wall, strength, how far from the tip they start, where toward the
    /// ring they fade in.
    t_along: f32,
    t_across: f32,
    t_warp: f32,
    t_sharp: f32,
    t_reach: f32,
    t_amp: f32,
    t_tip: f32,
    t_ring: f32,
    /// The folds across the wall's middle.
    m_along: f32,
    m_across: f32,
    m_amp: f32,
    /// The large folds gathered round the ring (an octabox's).
    lf_along: f32,
    lf_across: f32,
    lf_amp: f32,
    lf_reach: f32,
    /// The fine crinkle: frequency (per short half size) and strength.
    crinkle_density: f32,
    crinkle_amp: f32,
    /// The rods' soft shadows: strength and width.
    rod: f32,
    rod_soft: f32,
    /// A soft sheen over the form (its share of the walls' mean at the ring).
    sheen: f32,
    /// The throat: the back's edge blends over `throat_soft`, down to
    /// `throat_floor` of the walls' level.
    throat_soft: f32,
    throat_floor: f32,
    /// How far the plate's profile is lifted toward its mean (0 none).
    lift: f32,
    /// Darkening toward the lamp after the level: `c_floor` at the ring,
    /// none from `c_reach` of the way out.
    c_floor: f32,
    c_reach: f32,
    /// The walls' level relative to a face whose tube and walls share its
    /// light (an octabox's walls read darker on its plate).
    wall_scale: f32,
}

const SOFTBOX: Look = Look {
    depth: 1.5,
    back: 0.05,
    lamp_z: 0.8,
    view_z: -6.0,
    k_emit: 0.8,
    emit_floor: 0.3,
    kd: 0.15,
    lobes: [(0.5, 6.0), (0.8, 30.0)],
    t_along: 1.2,
    t_across: 2.5,
    t_warp: 0.6,
    t_sharp: 0.2,
    t_reach: 0.9,
    t_amp: 0.04,
    t_tip: 0.18,
    t_ring: 0.25,
    m_along: 0.9,
    m_across: 3.0,
    m_amp: 0.02,
    lf_along: 1.5,
    lf_across: 1.6,
    lf_amp: 0.0,
    lf_reach: 0.5,
    crinkle_density: 12.0,
    crinkle_amp: 0.045,
    rod: 0.6,
    rod_soft: 0.05,
    sheen: 0.08,
    throat_soft: 0.05,
    throat_floor: 0.5,
    lift: 0.0,
    c_floor: 0.1,
    c_reach: 0.6,
    wall_scale: 1.0,
};

const OCTABOX: Look = Look {
    depth: 1.5,
    back: 0.03,
    lamp_z: 0.8,
    view_z: -6.0,
    k_emit: 0.8,
    emit_floor: 0.3,
    kd: 0.05,
    lobes: [(0.5, 6.0), (1.2, 60.0)],
    t_along: 1.2,
    t_across: 2.5,
    t_warp: 0.35,
    t_sharp: 0.2,
    t_reach: 0.9,
    t_amp: 0.04,
    t_tip: 0.18,
    t_ring: 0.2,
    m_along: 0.5,
    m_across: 3.0,
    m_amp: 0.06,
    lf_along: 1.5,
    lf_across: 1.6,
    lf_amp: 0.1,
    lf_reach: 0.5,
    crinkle_density: 12.0,
    crinkle_amp: 0.045,
    rod: 0.6,
    rod_soft: 0.05,
    sheen: 0.0,
    throat_soft: 0.05,
    throat_floor: 0.8,
    lift: 0.1,
    c_floor: 1.0,
    c_reach: 0.3,
    wall_scale: 0.9,
};

/// The plates' brightness from the ring (0) to the opening (1.05), 40 bins,
/// smoothed over five, over their mean: the open softbox (RLS012) and the
/// open octabox (RLS019), measured over the whole face.
const PLATE_SOFTBOX: [f32; 40] = [
    0.3450, 0.3450, 0.3450, 0.3450, 0.3303, 0.3087, 0.2818, 0.2550, 0.2363, 0.2793, 0.3722, 0.5357, 0.7550, 0.8189, 0.8319, 0.8321, 0.8231, 0.8050, 0.9593, 1.0678, 1.1762, 1.1535, 1.0988, 1.0336, 0.9858, 1.0429, 1.1881, 1.3949, 1.5818, 1.7727, 1.7617, 1.7770, 1.7122, 1.6576, 1.5013, 1.2134, 0.8800, 0.5494, 0.2137, 0.0081,
];
const PLATE_OCTABOX: [f32; 40] = [
    0.7075, 0.7075, 0.7075, 0.7075, 0.6604, 0.5896, 0.5071, 0.4245, 0.3302, 0.2948, 0.3066, 0.3538, 0.4127, 0.4717, 0.5189, 0.5778, 0.6250, 0.7429, 0.9434, 1.1085, 1.4505, 1.8160, 1.9929, 2.0165, 2.0047, 1.7689, 1.4858, 1.3208, 1.2028, 1.1792, 1.1792, 1.1557, 1.0849, 1.0731, 1.0613, 0.8608, 0.6604, 0.4835, 0.2830, 0.0590,
];
const BINS: usize = 40;
const BIN_SPAN: f32 = 1.05;
/// How round a fold's crest is (a soft absolute value) and how much the
/// planes between crests undulate: without them the light turns each plane
/// into one flat band.
const FOLD_SOFT: f32 = 0.45;
const FOLD_UNDULATE: f32 = 0.25;
/// Samples per axis of the level tables (`OpenBox::new`).
const TABLE_SAMPLES: usize = 72;

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

type V3 = [f32; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn norm(a: V3) -> V3 {
    let l = dot(a, a).sqrt().max(1.0e-12);
    [a[0] / l, a[1] / l, a[2] / l]
}

fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// Ridged folds, 0..about 1, as the face's `folds` but with the crest
/// rounded (`FOLD_SOFT`) and the planes undulating (`FOLD_UNDULATE`).
fn soft_folds(x: f32, y: f32, seed: u32) -> f32 {
    let (mut px, mut py, mut amplitude, mut sum) = (x, y, 0.5f32, 0.0f32);
    for octave in 0..3u32 {
        let n = value2(px, py, seed.wrapping_add(octave.wrapping_mul(0x9e37_79b9)));
        let ridge = 1.0 - ((2.0 * n - 1.0).powi(2) + FOLD_SOFT * FOLD_SOFT).sqrt() + FOLD_SOFT;
        sum += ridge * ridge * amplitude;
        amplitude *= 0.5;
        let (rx, ry) = (0.8 * px - 0.6 * py, 0.6 * px + 0.8 * py);
        px = rx * 2.0 + 17.13;
        py = ry * 2.0 + 5.71;
    }
    (sum / 0.875).min(1.0) + FOLD_UNDULATE * (fbm2(x * 1.7 + 9.1, y * 1.7 + 2.3, 3, seed.wrapping_add(77)) - 0.5)
}

/// The value of `table` (40 bins over 0..`BIN_SPAN`) at `t`, linear between
/// bin middles and held past the ends.
fn lookup(table: &[f32; BINS], t: f32) -> f32 {
    let x = t / BIN_SPAN * BINS as f32 - 0.5;
    if x <= 0.0 {
        return table[0];
    }
    let i = x.floor() as usize;
    if i >= BINS - 1 {
        return table[BINS - 1];
    }
    let f = x - i as f32;
    table[i] + (table[i + 1] - table[i]) * f
}

/// A five-bin box smoothing, the ends held.
fn smooth_bins(v: &[f32; BINS]) -> [f32; BINS] {
    std::array::from_fn(|i| (0..5).map(|k| v[(i + k).saturating_sub(2).min(BINS - 1)]).sum::<f32>() / 5.0)
}

/// One wall of the box: its half width at the opening (`ho`), the back's
/// share of it, its slant length from the ring to the opening, and its seed.
#[derive(Clone, Copy)]
struct Wall {
    ho: f32,
    back: f32,
    length: f32,
    seed: u32,
}

impl Wall {
    fn new(ho: f32, back: f32, depth: f32, half_depth: f32, seed: u32) -> Wall {
        Wall { ho, back, length: depth.hypot((1.0 - back) * half_depth), seed }
    }

    /// The wall's half width `a` along its slant from the ring.
    fn half_width(&self, a: f32) -> f32 {
        self.ho * (self.back + (1.0 - self.back) * a / self.length)
    }
}

/// Where a point of the face falls in the box: the box distance `t` (0 at
/// the middle, 1 at the opening's edge), and on a wall, which wall, its
/// direction outward (`e`), the depth of the box along it, the point's slant
/// and across coordinates (`a`, `b`), and its position in the box's frame.
struct Place {
    t: f32,
    wall: Option<(Wall, [f32; 2], f32, f32, f32, f32, f32)>,
    seam: f32,
}

#[derive(Clone, Debug)]
pub struct OpenBox {
    octa: bool,
    aspect: Vec2f,
    look: Look,
    seed: u32,
    /// The flat walls' level per ring bin (before the plate's profile).
    ours: [f32; BINS],
    /// The plate's profile, lifted.
    target: [f32; BINS],
    /// The shaded walls' mean before the sheen and before the back (`walls_mean`, `back_mean`).
    walls_mean: f32,
    back_mean: f32,
    /// The face's mean after the level, which `walls` divides by.
    face_mean: f32,
    /// The tube's share of the face's light.
    share: f32,
}

impl OpenBox {
    /// An open softbox or strip (`octa` false: four walls on a box of
    /// `aspect`, its shorter side 1) or octabox (eight segments), its tube
    /// holding `share` of the light.
    pub fn new(octa: bool, aspect: Vec2f, share: f32, seed: u32) -> OpenBox {
        let look = if octa { OCTABOX } else { SOFTBOX };
        let plate = if octa { &PLATE_OCTABOX } else { &PLATE_SOFTBOX };
        let lifted_mean = plate[5..35].iter().sum::<f32>() / 30.0;
        let target = std::array::from_fn(|i| plate[i] * (1.0 - look.lift) + look.lift * lifted_mean);
        let mut b = OpenBox { octa, aspect, look, seed, ours: [1.0; BINS], target, walls_mean: 1.0, back_mean: 1.0, face_mean: 1.0, share };
        // The tables, sampled over the face: the shaded walls' means (with
        // the folds), the flat walls' level per ring (without), then the
        // face's mean after the level.
        let n = TABLE_SAMPLES;
        let points: Vec<Vec2f> = (0..n * n).map(|k| vec2f(((k % n) as f32 + 0.5) / n as f32 * 2.0 - 1.0, ((k / n) as f32 + 0.5) / n as f32 * 2.0 - 1.0)).collect();
        let places: Vec<Place> = points.iter().map(|&u| b.place(u)).collect();
        let shaded: Vec<Option<f32>> = places.iter().map(|p| b.shaded(p, false)).collect();
        let flat: Vec<Option<f32>> = places.iter().map(|p| b.shaded(p, true)).collect();
        let mean = |v: &[f32]| if v.is_empty() { 1.0 } else { v.iter().sum::<f32>() / v.len() as f32 };
        let walls: Vec<f32> = shaded.iter().flatten().copied().collect();
        b.walls_mean = mean(&walls).max(1.0e-9);
        let sheened: Vec<f32> = places.iter().zip(&shaded).filter_map(|(p, s)| s.map(|s| b.sheened(s, p.t))).collect();
        b.back_mean = mean(&sheened).max(1.0e-9);
        let (mut sum, mut count) = ([0.0f32; BINS], [0u32; BINS]);
        for (p, s) in places.iter().zip(&flat) {
            if let Some(s) = s {
                let i = ((p.t / BIN_SPAN * BINS as f32) as usize).min(BINS - 1);
                sum[i] += b.throated(b.sheened(*s, p.t), p.t);
                count[i] += 1;
            }
        }
        let filled: Vec<(usize, f32)> = (0..BINS).filter(|&i| count[i] > 0).map(|i| (i, sum[i] / count[i] as f32)).collect();
        if !filled.is_empty() {
            let raw: [f32; BINS] = std::array::from_fn(|i| {
                let after = filled.iter().position(|&(j, _)| j >= i);
                match after {
                    Some(0) => filled[0].1,
                    None => filled[filled.len() - 1].1,
                    Some(k) => {
                        let ((j0, v0), (j1, v1)) = (filled[k - 1], filled[k]);
                        if j1 == i {
                            v1
                        } else {
                            v0 + (v1 - v0) * (i - j0) as f32 / (j1 - j0) as f32
                        }
                    }
                }
            });
            b.ours = smooth_bins(&raw).map(|v| v.max(1.0e-9));
        }
        let finals: Vec<f32> = places.iter().zip(&shaded).filter(|(p, _)| p.t <= 1.0).map(|(p, s)| b.finished(p, *s)).collect();
        b.face_mean = mean(&finals).max(1.0e-9);
        b
    }

    /// Where `u` (box units, -1..1) falls: its ring, its wall and the frame.
    fn place(&self, u: Vec2f) -> Place {
        let look = &self.look;
        if self.octa {
            let sector = std::f32::consts::TAU / 8.0;
            let an = u.x.atan2(u.y);
            let segment = (an / sector).floor();
            let off = an - (segment + 0.5) * sector;
            let side_dist = std::f32::consts::FRAC_PI_8.cos();
            let rr = u.length();
            let t = rr * off.cos() / side_dist;
            let seam = (rr * (an - (an / sector).round() * sector).sin()).abs();
            if t < look.back || t > 1.0 {
                return Place { t, wall: None, seam };
            }
            let id = (segment as i32).rem_euclid(8) as u32;
            let e_ang = (id as f32 + 0.5) * sector;
            let half_side = std::f32::consts::FRAC_PI_8.tan() * side_dist;
            let wall = Wall::new(half_side, look.back, look.depth, side_dist, hash_u32(self.seed ^ id.wrapping_mul(0x9e37_79b9)));
            let a = (t - look.back) / (1.0 - look.back) * wall.length;
            Place { t, wall: Some((wall, [e_ang.sin(), e_ang.cos()], side_dist, a, rr * off.sin(), u.x, u.y)), seam }
        } else {
            let (hx, hy) = (self.aspect.x, self.aspect.y);
            let t = u.x.abs().max(u.y.abs());
            let seam = (u.x.abs() - u.y.abs()).abs() * std::f32::consts::FRAC_1_SQRT_2;
            if t < look.back || t > 1.0 {
                return Place { t, wall: None, seam };
            }
            let on_x = u.x.abs() > u.y.abs();
            let (id, e, half_depth, half_across, across) = if on_x {
                (u32::from(u.x < 0.0), [u.x.signum(), 0.0], hx, hy, u.y * hy)
            } else {
                (2 + u32::from(u.y < 0.0), [0.0, u.y.signum()], hy, hx, u.x * hx)
            };
            let wall = Wall::new(half_across, look.back, look.depth, half_depth, hash_u32(self.seed ^ id.wrapping_mul(0x9e37_79b9)));
            let a = (t - look.back) / (1.0 - look.back) * wall.length;
            Place { t, wall: Some((wall, e, half_depth, a, across, u.x * hx, u.y * hy)), seam }
        }
    }

    /// The fabric's height at slant `a` and across `b` on `wall`.
    fn height(&self, wall: &Wall, a: f32, b: f32) -> f32 {
        let look = &self.look;
        let (l, seed, ho) = (wall.length, wall.seed, wall.ho);
        let mut h = 0.0f32;
        let mut weights = 0.0f32;
        // The tension folds fanning from both rod tips toward the ring.
        for (k, c) in [-1.0f32, 1.0].into_iter().enumerate() {
            let k = k as u32;
            let (ta, tb) = (l, c * ho);
            let (da, db) = (a - ta, b - tb);
            let r = da.hypot(db) + 1.0e-6;
            let theta = db.atan2(da) - (-tb).atan2(-ta);
            let along = r / ho * look.t_along;
            let across = theta * look.t_across;
            let wa = fbm2(along * 0.6 + 3.1 + k as f32 * 11.0, across * 0.15 + 1.7, 3, seed.wrapping_add(31 + k));
            let wb = fbm2(along * 0.6 + 8.3 + k as f32 * 11.0, across * 0.15 + 4.2, 3, seed.wrapping_add(41 + k));
            let ua = along + look.t_warp * (wa - 0.5) * 4.0;
            let ub = across + look.t_warp * (wb - 0.5) * 4.0 * look.t_across * 0.3;
            let v = ((fbm2(ua, ub, 3, seed.wrapping_add(51 + k)) - 0.5) * 1.6 + 0.5).clamp(0.0, 1.0);
            let ridge = 1.0 - (v * 2.0 - 1.0).abs();
            let profile = look.t_sharp * ridge + (1.0 - look.t_sharp) * v;
            let reach = (-r / (look.t_reach * l)).exp() * (1.0 - (-(r / (look.t_tip * ho)).powi(2)).exp()) * smooth(look.t_ring, look.t_ring + 0.25, a / l);
            let w = 1.0 / (r * r);
            h += w * profile * reach;
            weights += w;
        }
        h = look.t_amp * ho * h / weights;
        // Larger folds across the wall's middle.
        let s = (a / l).clamp(0.0, 1.0);
        let (along, across) = (b / ho * look.m_along, a / l * look.m_across);
        let wa = fbm2(along * 0.5 + 2.2, across * 0.5 + 9.1, 3, seed.wrapping_add(71));
        let v = fbm2(along + look.t_warp * (wa - 0.5) * 4.0, across + look.t_warp * (wa - 0.5) * 2.0, 3, seed.wrapping_add(81));
        let ridge = 1.0 - (((v - 0.5) * 1.6 + 0.5).clamp(0.0, 1.0) * 2.0 - 1.0).abs();
        let middle = (std::f32::consts::PI * s).sin().powf(1.5) * (1.0 - (b / wall.half_width(a)).powi(4)).max(0.0);
        h += look.m_amp * ho * smooth(0.55, 1.0, ridge) * middle;
        // The large folds gathered round the ring.
        if look.lf_amp > 0.0 {
            let (fa, fb) = (a / l * look.lf_along, b / ho * look.lf_across);
            let w = fbm2(fa * 0.7 + 1.3, fb * 0.7 + 6.1, 2, seed.wrapping_add(121)) - 0.5;
            h += look.lf_amp * ho * soft_folds(fa + 0.8 * w, fb + 0.8 * w, seed.wrapping_add(131)) * (-s / look.lf_reach).exp();
        }
        // The fine crinkle, stronger in patches.
        let fine = look.crinkle_density / ho;
        let patch = smooth(0.3, 0.7, fbm2(a / ho * 1.5 + 4.4, b / ho * 1.5 + 1.1, 3, seed.wrapping_add(91)));
        h + look.crinkle_amp * (0.25 + 0.75 * patch) * soft_folds(a * fine + 5.1, b * fine + 2.3, seed.wrapping_add(9)) / fine
    }

    /// The tube's light off the wall at `place`, folded (`flat` false) or
    /// not; None off the walls.
    fn shaded(&self, place: &Place, flat: bool) -> Option<f32> {
        let look = &self.look;
        let (wall, e, half_depth, a, b, px, py) = place.wall?;
        let q = ((1.0 - place.t) / (1.0 - look.back)).clamp(0.03, 1.0);
        let slope = -look.depth / ((1.0 - look.back) * half_depth);
        let n0 = norm([slope * e[0], slope * e[1], -1.0]);
        let along = norm([e[0], e[1], slope]);
        let across = cross(n0, along);
        let n = if flat {
            n0
        } else {
            let eps = 0.002;
            let h0 = self.height(&wall, a, b);
            let ha = (self.height(&wall, a + eps, b) - h0) / eps;
            let hb = (self.height(&wall, a, b + eps) - h0) / eps;
            norm([n0[0] - ha * along[0] - hb * across[0], n0[1] - ha * along[1] - hb * across[1], n0[2] - ha * along[2] - hb * across[2]])
        };
        let p = [px, py, look.depth * q];
        let to_light = sub([0.0, 0.0, look.depth * look.lamp_z], p);
        let d2 = dot(to_light, to_light).max(1.0e-9);
        let l = norm(to_light);
        let v = norm(sub([0.0, 0.0, look.view_z], p));
        let emit = look.emit_floor + (1.0 - look.emit_floor) * l[2].max(0.0).powf(look.k_emit);
        let irradiance = emit * dot(n, l).max(0.0) / d2;
        let h = norm([l[0] + v[0], l[1] + v[1], l[2] + v[2]]);
        let ndh = dot(n, h).max(0.0);
        let mut out = look.kd * irradiance / std::f32::consts::PI;
        for (ks, s) in look.lobes {
            out += ks * (s + 8.0) / (8.0 * std::f32::consts::PI) * ndh.powf(s) * irradiance;
        }
        // The rods along the corners, soft shadows.
        Some(out * (1.0 - look.rod * (1.0 - smooth(0.0, look.rod_soft, place.seam)).powi(2)))
    }

    /// The shaded wall with the sheen over the form.
    fn sheened(&self, shaded: f32, t: f32) -> f32 {
        shaded + self.look.sheen * self.walls_mean * (1.0 - t)
    }

    /// The throat: the back's edge blended down toward `throat_floor`.
    fn throated(&self, v: f32, t: f32) -> f32 {
        let look = &self.look;
        v * (look.throat_floor + (1.0 - look.throat_floor) * smooth(look.back - look.throat_soft, look.back + look.throat_soft, t))
    }

    /// The face at `place` before the normalisation: the walls (`shaded`)
    /// or the back, throated, at the plate's level, darkened toward the lamp.
    fn finished(&self, place: &Place, shaded: Option<f32>) -> f32 {
        let look = &self.look;
        let t = place.t;
        let v = match shaded {
            Some(s) => {
                let s = self.throated(self.sheened(s, t), t);
                s * lookup(&self.target, t) / lookup(&self.ours, t)
            }
            None if t < look.back => self.throated(0.03 * self.back_mean, t),
            None => 0.0,
        };
        v * (look.c_floor + (1.0 - look.c_floor) * smooth(0.0, look.c_reach, t))
    }

    /// The silver walls at `u` (box units), at a level that gives the walls
    /// 1 - `share` of a face whose mean is 1 (an octabox's `wall_scale` of
    /// that); 0 outside the box.
    pub fn walls(&self, u: Vec2f) -> f32 {
        if !u.x.is_finite() || !u.y.is_finite() {
            return 0.0;
        }
        let place = self.place(u);
        if place.t > 1.0 {
            return 0.0;
        }
        let shaded = self.shaded(&place, false);
        self.look.wall_scale * (1.0 - self.share) * self.finished(&place, shaded) / self.face_mean
    }
}
