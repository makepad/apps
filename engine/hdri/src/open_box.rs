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
//! round its ring; broad soft folds over the whole wall (the plates' cloud
//! of bright and dark patches); straight creases where the foil was folded
//! flat and opened again; a fine crinkle in patches. The surface normal
//! follows the height's slope, and the tube lights it: diffuse and two
//! glossy lobes, the tube shining mostly forward but also sideways onto the
//! walls.
//!
//! The brightness from the ring to the opening then follows the plate's
//! measured profile (the photographed open boxes RLS012 and RLS019,
//! smoothed), so the walls
//! sit where the real ones do while the lighting keeps the folds' detail;
//! the softbox is darkened toward the lamp after that. Over it all lies the
//! foil's glitter: grains finer than the folds, each a tiny facet at its own
//! tilt, log-uniform in brightness with mean 1 (the plates' grain). The level
//! tables are sampled once per light (`OpenBox::new`), not per texel.
//!
//! Per texel: three height evaluations (about thirty value-noise lookups
//! each) and the lighting, a few microseconds; only open boxes pay it.
use makepad_draw::*;

use super::modifier::{cells, log_uniform};
use super::noise::{fbm2, hash01, hash_u32};
use super::patterns::{fan_folds, patches};

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
    /// The tension folds fanning out from each rod's foot at the ring (the
    /// ring pulls the foil along the rods: the plates' pinched points of the
    /// star): strength, reach along the wall, across frequency (per radian of
    /// the fan) and how far the fan turns as it goes out (a log spiral, the
    /// ring a little twisted on the rods, so all the points lean one way).
    /// The along frequency, warp and crease profile are the tip fans'. 0 none.
    r_amp: f32,
    r_reach: f32,
    r_across: f32,
    r_lean: f32,
    /// The folds across the wall's middle.
    m_along: f32,
    m_across: f32,
    m_amp: f32,
    /// The large folds gathered round the ring (an octabox's).
    lf_along: f32,
    lf_across: f32,
    lf_amp: f32,
    lf_reach: f32,
    /// The broad soft folds over the whole wall, the bright and dark
    /// patches the plates show: frequency (per wall half width) and height.
    b_freq: f32,
    b_amp: f32,
    /// The creases where the foil was folded flat and opened again: `creases`
    /// straight hinges a wall, each bending the foil by up to `crease_amp` (a
    /// slope) either way, rounded over `crease_soft` of the wall's half width
    /// and flattening out `crease_reach` of it to either side.
    creases: u32,
    crease_amp: f32,
    crease_soft: f32,
    crease_reach: f32,
    /// The facets: the foil lies in broad planes, each at its own tilt
    /// (a slope up to `facet_amp`), `facet_freq` of them per wall half
    /// width, meeting in creases `facet_soft` of the half width wide.
    facet_freq: f32,
    facet_amp: f32,
    facet_soft: f32,
    /// The fine crinkle: frequency (per short half size) and strength.
    crinkle_density: f32,
    crinkle_amp: f32,
    /// The glitter of the foil's grain: grains per short half size, each a
    /// tiny flat facet whose brightness is log-uniform over e^`grain`.
    grain_density: f32,
    grain: f32,
    /// The rods' soft shadows: strength and width; toward the ring the
    /// fabric gathers along them, so the shadow widens by `gather` (a
    /// wedge, the plates' dark X or star round the tube) from
    /// `gather_reach` of the way out.
    rod: f32,
    rod_soft: f32,
    gather: f32,
    gather_reach: f32,
    /// The taut zone round the ring (the tension map: the ring and the rods
    /// pull the foil flat there, so it carries no folds and no grain and
    /// mirrors little of the tube to the camera, the plates' smooth dark
    /// rectangle or star round the tube): it ends at the box distance
    /// `taut_in` between the rods and `taut_in + taut_reach` along them, a
    /// straight wedge `taut_width` wide (the distance off the rod over the
    /// box distance), its edge `taut_soft` wide.
    taut_in: f32,
    taut_reach: f32,
    taut_width: f32,
    taut_soft: f32,
    /// How pinched each rod's point of the taut zone is: its reach falls off
    /// the rod as (1 - off / width)^`taut_pinch` (1 a straight wedge; more,
    /// the plates' narrow points with hollow sides).
    taut_pinch: f32,
    /// The taut zone's light over the flat wall's middle at the same box
    /// distance (1 none: the tube's own fall over the wall). Taut foil is a
    /// smooth mirror: it shows the dark box, not the tube, and is lit evenly by
    /// the bounce off the slack walls, its folds still shading it. So the
    /// plates' star is even round the ring and its points along the rods are
    /// darker than the slack foil beside them.
    taut_level: f32,
    /// How much of the large folds (the tension fans, the middle and gathered
    /// folds, the broad folds, facets and creases) the taut zone keeps: the
    /// plates show the foil there dark and grainless but still folded and
    /// creased, lit by the bounce. Its fine crinkle and grain go entirely.
    taut_folds: f32,
    /// How unevenly the taut zone reaches out along each rod (0 the same along
    /// every rod): each rod's reach and width vary by up to this share, so the
    /// octabox's star has the plates' uneven points.
    star: f32,
    /// The share of the foil's grain that the taut zone keeps (the plates show
    /// a little grain right up to the ring and the back panel).
    grain_floor: f32,
    /// The back panel round the ring (RLS012's dim rectangle, the rods
    /// meeting at its corners): inside the box distance `panel` the foil
    /// lies flat, facing the opening, lit at a grazing angle, one even level
    /// (the plate's darkest ring's) with the rods faint across it to the ring;
    /// its edge soft over a short span. 0 none (the walls run to the ring). On the face it
    /// spans `panel_at` of the opening's half sizes across and up (the back
    /// is square, the opening is not), the walls between it and the opening
    /// drawn as a frustum, the rods from its corners to the opening's.
    panel: f32,
    panel_at: [f32; 2],
    /// The panel's level over the face's mean; 0 takes the plate's level at
    /// the ring (the open softbox's darkest ring lies there, RLS012). The
    /// open octabox's lies further out (RLS019: 0.29 at 0.25 of the way out,
    /// 0.71 by the ring), so it sets its own.
    panel_level: f32,
    /// How much of its folds' light the panel shows (a power of their light
    /// over the flat walls', `PANEL_SOFT` for the open softbox) and the share
    /// of the foil's grain it keeps (1 all): RLS019's zone round the lamp is
    /// smooth, near flat and grainless.
    panel_soft: f32,
    panel_grain: f32,
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
    lobes: [(0.5, 6.0), (0.8, 12.0)],
    t_along: 1.2,
    t_across: 2.5,
    t_warp: 0.6,
    t_sharp: 0.2,
    t_reach: 0.9,
    t_amp: 0.012,
    t_tip: 0.18,
    t_ring: 0.25,
    r_amp: 0.0,
    r_reach: 0.0,
    r_across: 0.0,
    r_lean: 0.0,
    m_along: 0.9,
    m_across: 3.0,
    m_amp: 0.01,
    lf_along: 1.5,
    lf_across: 1.6,
    lf_amp: 0.0,
    lf_reach: 0.5,
    b_freq: 1.4,
    b_amp: 0.04,
    creases: 6,
    crease_amp: 0.08,
    crease_soft: 0.006,
    crease_reach: 0.15,
    facet_freq: 0.9,
    facet_amp: 0.1,
    facet_soft: 0.25,
    crinkle_density: 12.0,
    crinkle_amp: 0.02,
    grain_density: 240.0,
    grain: 1.0,
    rod: 0.6,
    rod_soft: 0.008,
    gather: 0.0,
    gather_reach: 0.45,
    taut_in: 0.27,
    taut_reach: 0.08,
    taut_width: 0.4,
    taut_soft: 0.06,
    taut_pinch: 1.0,
    taut_level: 1.0,
    taut_folds: 1.0,
    star: 0.0,
    grain_floor: 0.15,
    panel: 0.35,
    panel_at: [0.41, 0.28],
    panel_level: 0.0,
    panel_soft: PANEL_SOFT,
    panel_grain: 1.0,
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
    kd: 0.15,
    lobes: [(0.5, 6.0), (0.8, 30.0)],
    t_along: 1.2,
    t_across: 2.5,
    t_warp: 0.35,
    t_sharp: 0.2,
    t_reach: 0.9,
    t_amp: 0.004,
    t_tip: 0.18,
    t_ring: 0.2,
    r_amp: 0.05,
    r_reach: 0.35,
    r_across: 3.0,
    r_lean: 0.4,
    m_along: 0.5,
    m_across: 3.0,
    m_amp: 0.012,
    lf_along: 1.5,
    lf_across: 1.6,
    lf_amp: 0.012,
    lf_reach: 0.5,
    b_freq: 1.4,
    b_amp: 0.04,
    creases: 7,
    crease_amp: 0.1,
    crease_soft: 0.01,
    crease_reach: 0.12,
    facet_freq: 1.0,
    facet_amp: 0.12,
    facet_soft: 0.12,
    crinkle_density: 12.0,
    crinkle_amp: 0.012,
    grain_density: 240.0,
    grain: 1.3,
    rod: 0.6,
    rod_soft: 0.008,
    gather: 0.0,
    gather_reach: 0.35,
    taut_in: 0.32,
    taut_reach: 0.18,
    taut_width: 0.3,
    taut_soft: 0.025,
    taut_pinch: 2.5,
    taut_level: 0.15,
    taut_folds: 1.0,
    star: 0.4,
    grain_floor: 0.08,
    panel: 0.3,
    panel_at: [0.3, 0.3],
    panel_level: 0.3,
    panel_soft: 0.25,
    panel_grain: 0.0,
    sheen: 0.0,
    throat_soft: 0.05,
    throat_floor: 0.8,
    lift: 0.0,
    c_floor: 1.0,
    c_reach: 0.3,
    wall_scale: 0.9,
};

/// How much of its folds' light the back panel shows (a power of their
/// light over the flat walls'): softer than the walls, the panel lying flat.
const PANEL_SOFT: f32 = 1.5;

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

use crate::patterns::smooth;

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
    let crests = crate::noise::shaped_fbm2(x, y, 3, seed, |n| {
        let ridge = 1.0 - ((2.0 * n - 1.0).powi(2) + FOLD_SOFT * FOLD_SOFT).sqrt() + FOLD_SOFT;
        ridge * ridge
    });
    crests + FOLD_UNDULATE * (fbm2(x * 1.7 + 9.1, y * 1.7 + 2.3, 3, seed.wrapping_add(77)) - 0.5)
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
    /// The nearest rod (0..8 round an octabox, 0..4 at a box's corners).
    rod: u32,
}

#[derive(Clone, Debug)]
pub struct OpenBox {
    octa: bool,
    aspect: Vec2f,
    look: Look,
    seed: u32,
    /// The folded walls' level per ring bin (before the plate's profile).
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
    /// The back panel's half sizes on the face, shares of the opening's half
    /// sizes (an octabox's: its corner radius), where a box's 3D model
    /// (light_model_box.rs) puts its back.
    pub fn back_panel(octa: bool) -> [f32; 2] {
        if octa { OCTABOX.panel_at } else { SOFTBOX.panel_at }
    }

    /// An open softbox or strip (`octa` false: four walls on a box of
    /// `aspect`, its shorter side 1) or octabox (eight segments), its tube
    /// holding `share` of the light.
    pub fn new(octa: bool, aspect: Vec2f, share: f32, seed: u32) -> OpenBox {
        let look = if octa { OCTABOX } else { SOFTBOX };
        let plate = if octa { &PLATE_OCTABOX } else { &PLATE_SOFTBOX };
        let lifted_mean = plate[5..35].iter().sum::<f32>() / 30.0;
        // Inside its darkest ring the plate's profile is the tube's flare
        // over the taut foil (a camera effect, never baked): the level holds
        // at that darkest ring's there.
        let darkest = (0..BINS / 2).min_by(|&i, &j| plate[i].total_cmp(&plate[j])).unwrap_or(0);
        let target = std::array::from_fn(|i| plate[i.max(darkest)] * (1.0 - look.lift) + look.lift * lifted_mean);
        let mut b = OpenBox { octa, aspect, look, seed, ours: [1.0; BINS], target, walls_mean: 1.0, back_mean: 1.0, face_mean: 1.0, share };
        // The tables, sampled over the face: the shaded walls' means (with
        // the folds), the folded walls' level per ring (what is drawn: a
        // flat wall's level is near nothing by the ring, where the tube
        // lights it at a grazing angle, and lifting the folds there by that
        // ratio blew them out), then the face's mean after the level.
        let n = TABLE_SAMPLES;
        let points: Vec<Vec2f> = (0..n * n).map(|k| vec2f(((k % n) as f32 + 0.5) / n as f32 * 2.0 - 1.0, ((k / n) as f32 + 0.5) / n as f32 * 2.0 - 1.0)).collect();
        let places: Vec<Place> = points.iter().map(|&u| b.place(u)).collect();
        let shaded: Vec<Option<f32>> = places.iter().map(|p| b.shaded(p, false)).collect();
        let mean = |v: &[f32]| if v.is_empty() { 1.0 } else { v.iter().sum::<f32>() / v.len() as f32 };
        let walls: Vec<f32> = shaded.iter().flatten().copied().collect();
        b.walls_mean = mean(&walls).max(1.0e-9);
        let sheened: Vec<f32> = places.iter().zip(&shaded).filter_map(|(p, s)| s.map(|s| b.sheened(s, p.t))).collect();
        b.back_mean = mean(&sheened).max(1.0e-9);
        let (mut sum, mut count) = ([0.0f32; BINS], [0u32; BINS]);
        for (p, s) in places.iter().zip(&shaded) {
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
            let rod = ((an / sector).round() as i32).rem_euclid(8) as u32;
            if t < look.back || t > 1.0 {
                return Place { t, wall: None, seam, rod };
            }
            let id = (segment as i32).rem_euclid(8) as u32;
            let e_ang = (id as f32 + 0.5) * sector;
            let half_side = std::f32::consts::FRAC_PI_8.tan() * side_dist;
            let wall = Wall::new(half_side, look.back, look.depth, side_dist, hash_u32(self.seed ^ id.wrapping_mul(0x9e37_79b9)));
            let a = (t - look.back) / (1.0 - look.back) * wall.length;
            Place { t, wall: Some((wall, [e_ang.sin(), e_ang.cos()], side_dist, a, rr * off.sin(), u.x, u.y)), seam, rod }
        } else {
            let (hx, hy) = (self.aspect.x, self.aspect.y);
            let t = u.x.abs().max(u.y.abs());
            let seam = (u.x.abs() - u.y.abs()).abs() * std::f32::consts::FRAC_1_SQRT_2;
            let rod = u32::from(u.x < 0.0) + 2 * u32::from(u.y < 0.0);
            if t < look.back || t > 1.0 {
                return Place { t, wall: None, seam, rod };
            }
            let on_x = u.x.abs() > u.y.abs();
            let (id, e, half_depth, half_across, across) = if on_x {
                (u32::from(u.x < 0.0), [u.x.signum(), 0.0], hx, hy, u.y * hy)
            } else {
                (2 + u32::from(u.y < 0.0), [0.0, u.y.signum()], hy, hx, u.x * hx)
            };
            let wall = Wall::new(half_across, look.back, look.depth, half_depth, hash_u32(self.seed ^ id.wrapping_mul(0x9e37_79b9)));
            let a = (t - look.back) / (1.0 - look.back) * wall.length;
            Place { t, wall: Some((wall, e, half_depth, a, across, u.x * hx, u.y * hy)), seam, rod }
        }
    }

    /// The fabric's height at slant `a` and across `b` on `wall`: its large
    /// folds and, apart, its fine crinkle (which the taut zone drops
    /// entirely, `taut_folds`).
    fn height(&self, wall: &Wall, a: f32, b: f32) -> (f32, f32) {
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
            let seeds = [seed.wrapping_add(31 + k), seed.wrapping_add(41 + k), seed.wrapping_add(51 + k)];
            let profile = fan_folds(along, across, look.t_warp, look.t_across, look.t_sharp, [3.1, 1.7, 8.3, 4.2], k as f32 * 11.0, seeds);
            let reach = (-r / (look.t_reach * l)).exp() * (1.0 - (-(r / (look.t_tip * ho)).powi(2)).exp()) * smooth(look.t_ring, look.t_ring + 0.25, a / l);
            let w = 1.0 / (r * r);
            h += w * profile * reach;
            weights += w;
        }
        h = look.t_amp * ho * h / weights;
        // The tension folds fanning out from the rods' feet at the ring.
        if look.r_amp > 0.0 {
            let (mut hr, mut wr) = (0.0f32, 0.0f32);
            for (k, c) in [-1.0f32, 1.0].into_iter().enumerate() {
                let k = k as u32;
                let tb = c * wall.half_width(0.0);
                let (da, db) = (a, b - tb);
                let r = da.hypot(db) + 1.0e-6;
                // The angle off the rod (its foot to its tip), turning as it goes out.
                let theta = db.atan2(da) - (c * ho - tb).atan2(l) - look.r_lean * (r / ho).ln();
                let along = r / ho * look.t_along;
                let across = theta * look.r_across;
                let seeds = [seed.wrapping_add(201 + k), seed.wrapping_add(211 + k), seed.wrapping_add(221 + k)];
                let profile = fan_folds(along, across, look.t_warp, look.r_across, look.t_sharp, [6.7, 2.9, 1.9, 7.4], k as f32 * 11.0, seeds);
                let reach = (-r / (look.r_reach * l)).exp() * (1.0 - (-(r / (look.t_tip * ho)).powi(2)).exp());
                let w = 1.0 / (r * r);
                hr += w * profile * reach;
                wr += w;
            }
            h += look.r_amp * ho * hr / wr;
        }
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
        // The broad soft folds: the walls' bright and dark patches.
        let (ba, bb) = (a / ho * look.b_freq, b / ho * look.b_freq);
        h += look.b_amp * ho * (fbm2(ba + 4.1, bb + 0.6, 3, seed.wrapping_add(161)) - 0.5);
        // The facets: broad planes meeting in soft creases.
        h += self.facets(wall, a, b);
        // The creases: straight hinges.
        h += self.creases(wall, a, b);
        // The fine crinkle, stronger in patches.
        let fine = look.crinkle_density / ho;
        let patch = patches(a / ho, b / ho, 1.5, 0.3, 0.7, 3, seed.wrapping_add(91));
        (h, look.crinkle_amp * (0.25 + 0.75 * patch) * soft_folds(a * fine + 5.1, b * fine + 2.3, seed.wrapping_add(9)) / fine)
    }

    /// The straight creases of `wall` at slant `a` and across `b`: each a
    /// hinge along a line through a hashed point of the wall at a hashed
    /// angle, as long as 0.5 to 1.5 of the wall's half width (fading at its
    /// ends), bending the foil up or down by its own share of `crease_amp`
    /// and flattening out to either side: a crisp line with a lit and a
    /// shaded facet beside it, as the plates show them.
    fn creases(&self, wall: &Wall, a: f32, b: f32) -> f32 {
        let look = &self.look;
        let (l, ho) = (wall.length, wall.ho);
        let soft = look.crease_soft * ho;
        let reach = look.crease_reach * ho;
        let mut h = 0.0f32;
        for i in 0..look.creases as i32 {
            let k = |j: i32| hash01(i, j, 7, wall.seed);
            let pa = (0.15 + 0.85 * k(0)) * l;
            let pb = (2.0 * k(1) - 1.0) * 0.8 * wall.half_width(pa);
            let (sa, ca) = (std::f32::consts::PI * k(2)).sin_cos();
            let (da, db) = (a - pa, b - pb);
            let across = da * sa - db * ca;
            let along = da * ca + db * sa;
            let half_length = (0.25 + 0.5 * k(3)) * ho;
            let window = 1.0 - smooth(0.3 * half_length, half_length, along.abs());
            let bend = look.crease_amp * (0.4 + 0.8 * k(4)) * if k(5) < 0.5 { -1.0 } else { 1.0 };
            let flatten = (-(across / reach).powi(2)).exp();
            h += bend * ((across * across + soft * soft).sqrt() - soft) * flatten * window;
        }
        h
    }

    /// The facets of `wall` at slant `a` and across `b`: the foil, folded
    /// flat and opened again, lies in broad planes (one per cell of a
    /// jittered lattice, `facet_freq` cells per half width), each tilted
    /// its own way by up to `facet_amp`, blended between neighbours over
    /// `facet_soft` of the half width (a smooth Voronoi of planes): a
    /// broad lit or shaded facet each, meeting in crisp creases, as the
    /// plates' walls show.
    fn facets(&self, wall: &Wall, a: f32, b: f32) -> f32 {
        let look = &self.look;
        if look.facet_amp <= 0.0 {
            return 0.0;
        }
        let ho = wall.ho;
        let f = look.facet_freq / ho;
        let (x, y) = (a * f + 0.37, b * f + 2.9);
        let (ix, iy) = (x.floor(), y.floor());
        let soft = (look.facet_soft * ho * f).max(1.0e-3);
        // Five by five cells: each plane's weight falls to nothing `FACET_CUT`
        // of a cell further than the nearest point, and a cell outside the
        // five by five is at least 2 away, the nearest point almost always
        // under 1.1 (a jittered lattice leaves no wider gap), so a point
        // crossing a cell's border never drops a plane that still weighs
        // (the three by three did: a step in the height there, its slope a
        // black line of dots along each border).
        const FACET_CUT: f32 = 0.9;
        let mut planes = [(0.0f32, 0.0f32); 25];
        let mut nearest = f32::INFINITY;
        let mut k = 0;
        for j in -2..=2i32 {
            for i in -2..=2i32 {
                let (cx, cy) = (ix as i32 + i, iy as i32 + j);
                let px = ix + i as f32 + hash01(cx, cy, 0, wall.seed ^ 0x7f1);
                let py = iy + j as f32 + hash01(cx, cy, 1, wall.seed ^ 0x7f1);
                let (dx, dy) = (x - px, y - py);
                let d = dx.hypot(dy);
                let ga = 2.0 * hash01(cx, cy, 2, wall.seed ^ 0x7f1) - 1.0;
                let gb = 2.0 * hash01(cx, cy, 3, wall.seed ^ 0x7f1) - 1.0;
                planes[k] = (d, (ga * dx + gb * dy) / f);
                k += 1;
                nearest = nearest.min(d);
            }
        }
        let (mut sum, mut weights) = (0.0f32, 0.0f32);
        let floor = (-FACET_CUT / soft).exp();
        for (d, plane) in planes {
            let w = ((-(d - nearest) / soft).exp() - floor).max(0.0);
            sum += w * plane;
            weights += w;
        }
        look.facet_amp * sum / weights.max(1.0e-9)
    }

    /// How slack the foil is at `place` (the tension map: 0 in the taut
    /// zone round the ring, reaching further out along the rods, 1 beyond
    /// it), what scales its folds and its grain.
    fn slack(&self, place: &Place) -> f32 {
        let look = &self.look;
        let off = place.seam / place.t.max(1.0e-3);
        // Each rod's point of the star reaches and spreads its own way.
        let k = |j: i32| 1.0 + look.star * (2.0 * hash01(place.rod as i32, j, 11, self.seed ^ 0x51a) - 1.0);
        let point = (1.0 - off / (look.taut_width * k(1))).max(0.0).powf(look.taut_pinch);
        let edge = look.taut_in + look.taut_reach * k(0) * point;
        smooth(edge, edge + look.taut_soft, place.t)
    }

    /// The tube's light off the wall at `place`, folded (`flat` false) or
    /// not, with the taut zone's even light and the rods' shadows; None off
    /// the walls.
    fn shaded(&self, place: &Place, flat: bool) -> Option<f32> {
        let look = &self.look;
        let out = self.lit(place, flat)?;
        let mut v = out;
        if look.taut_level < 1.0 {
            let slack = self.slack(place);
            if slack < 1.0 {
                // The flat wall's middle at this box distance, and the folds'
                // light over the flat wall's here, so the taut foil keeps them.
                let (_, e, ..) = place.wall?;
                let r = place.t * if self.octa { std::f32::consts::FRAC_PI_8.cos() } else { 1.0 };
                let middle = self.lit(&self.place(vec2f(e[0] * r, e[1] * r)), true).unwrap_or(out);
                let folds = if flat {
                    1.0
                } else {
                    match self.lit(place, true) {
                        Some(f) if f > 1.0e-9 => (out / f).min(6.0),
                        _ => 1.0,
                    }
                };
                let even = look.taut_level * middle * folds;
                v = even + (out - even) * slack;
            }
        }
        // The rods along the corners, soft shadows, widening into the
        // gathered fabric's dark wedges toward the ring.
        let width = look.rod_soft + look.gather * (1.0 - smooth(look.back, look.gather_reach, place.t)).powi(2);
        Some(v * (1.0 - look.rod * (1.0 - smooth(0.0, width, place.seam)).powi(2)))
    }

    /// The tube's light off the wall at `place`, folded or not, before the
    /// taut zone's even light and the rods.
    fn lit(&self, place: &Place, flat: bool) -> Option<f32> {
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
            // The tension map: the taut zone keeps `taut_folds` of the large
            // folds' slope and none of the crinkle's.
            let slack = self.slack(place);
            let keep = look.taut_folds + (1.0 - look.taut_folds) * slack;
            let slope = |(c1, f1): (f32, f32), (c0, f0): (f32, f32)| (keep * (c1 - c0) + slack * (f1 - f0)) / eps;
            let h0 = self.height(&wall, a, b);
            let ha = slope(self.height(&wall, a + eps, b), h0);
            let hb = slope(self.height(&wall, a, b + eps), h0);
            norm([n0[0] - ha * along[0] - hb * across[0], n0[1] - ha * along[1] - hb * across[1], n0[2] - ha * along[2] - hb * across[2]])
        };
        let p = [px, py, look.depth * q];
        let to_light = sub([0.0, 0.0, look.depth * look.lamp_z], p);
        let d2 = dot(to_light, to_light).max(1.0e-9);
        let l = norm(to_light);
        let v = norm(sub([0.0, 0.0, look.view_z], p));
        let emit = look.emit_floor + (1.0 - look.emit_floor) * l[2].max(0.0).powf(look.k_emit);
        // A fold or crease turned away from the tube still gets the light the
        // other walls bounce onto it: part of the light comes in as though the
        // foil lay flat, so no facet goes black (the plates show none). The
        // unfolded wall (`flat`) is unchanged.
        const BOUNCE: f32 = 0.3;
        let irradiance = emit * (dot(n, l).max(0.0) + BOUNCE * dot(n0, l).max(0.0)) / ((1.0 + BOUNCE) * d2);
        let h = norm([l[0] + v[0], l[1] + v[1], l[2] + v[2]]);
        let ndh = dot(n, h).max(0.0);
        let mut out = look.kd * irradiance / std::f32::consts::PI;
        for (ks, s) in look.lobes {
            out += ks * (s + 8.0) / (8.0 * std::f32::consts::PI) * ndh.powf(s) * irradiance;
        }
        Some(out)
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

    /// The walls' folds at `u` (box units): their light over the same wall's
    /// light unfolded there, with the foil's grain. The lining's texture
    /// without its level or the tube's fall over each wall (mean about 1),
    /// for a face that sets each wall's level itself (the inner baffle's
    /// band, the same walls seen between the inner cloth and the opening).
    /// 1 off the walls.
    pub fn folds(&self, u: Vec2f) -> f32 {
        if !u.x.is_finite() || !u.y.is_finite() {
            return 1.0;
        }
        let place = self.place(u);
        if place.t > 1.0 {
            return 1.0;
        }
        match (self.shaded(&place, false), self.shaded(&place, true)) {
            (Some(folded), Some(flat)) if flat > 1.0e-9 => (folded / flat).min(6.0) * self.glitter(u, self.slack(&place)),
            _ => 1.0,
        }
    }

    /// The face at `place` before the normalisation: the walls (`shaded`)
    /// or the back, throated, at the plate's level, darkened toward the lamp.
    fn finished(&self, place: &Place, shaded: Option<f32>) -> f32 {
        let look = &self.look;
        let t = place.t;
        let v = match shaded {
            Some(s) => {
                let s_raw = s;
                let s = self.throated(self.sheened(s, t), t);
                let wall = s * lookup(&self.target, t) / lookup(&self.ours, t);
                if look.panel > 0.0 {
                    const PANEL_EDGE: f32 = 0.025;
                    let rods = 1.0 - 0.5 * look.rod * (1.0 - smooth(0.0, look.rod_soft, place.seam)).powi(2);
                    // The panel's cloth: its folds lit as the walls' are (their
                    // light over the same wall's unfolded), softened, at the
                    // plate's darkest ring's level.
                    let cloth = match (t < look.panel + PANEL_EDGE, self.shaded(place, true)) {
                        (true, Some(flat)) if flat > 1.0e-9 => (s_raw / flat).clamp(0.2, 5.0).powf(look.panel_soft),
                        _ => 1.0,
                    };
                    let level = if look.panel_level > 0.0 { look.panel_level } else { self.target[0] };
                    let panel = level * rods * cloth;
                    panel + (wall - panel) * smooth(look.panel - PANEL_EDGE, look.panel + PANEL_EDGE, t)
                } else {
                    wall
                }
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
        let place = self.place(self.frustum(u));
        if place.t > 1.0 {
            return 0.0;
        }
        let shaded = self.shaded(&place, false);
        let look = &self.look;
        let mut slack = self.slack(&place);
        if look.panel > 0.0 && look.panel_grain < 1.0 {
            // The panel keeps `panel_grain` of the grain.
            slack *= look.panel_grain + (1.0 - look.panel_grain) * smooth(look.panel - 0.025, look.panel + 0.025, place.t);
        }
        look.wall_scale * (1.0 - self.share) * self.finished(&place, shaded) * self.glitter(u, slack) / self.face_mean
    }

    /// Where a face point `u` falls on the walls when the back panel is not
    /// the opening's shape (`panel_at`): each axis stretched so the panel's
    /// edge falls on the box distance `panel`, straight from there to the
    /// opening; `u` itself without a panel.
    fn frustum(&self, u: Vec2f) -> Vec2f {
        let look = &self.look;
        if look.panel <= 0.0 {
            return u;
        }
        let map = |v: f32, at: f32| {
            let a = v.abs();
            let m = if a < at { a * look.panel / at } else { look.panel + (a - at) * (1.0 - look.panel) / (1.0 - at) };
            m * v.signum()
        };
        vec2f(map(u.x, look.panel_at[0]), map(u.y, look.panel_at[1]))
    }

    /// The foil's grain at `u`: one tiny flat facet per grain, mirroring the
    /// tube at its own tilt (the face's facets, `log_uniform`, finer), its
    /// spread scaled by the foil's `slack` (taut foil lies flat). Mean 1.
    fn glitter(&self, u: Vec2f, slack: f32) -> f32 {
        let g = self.look.grain_density;
        let (_, _, tilt) = cells(u.x * self.aspect.x * g + 0.37, u.y * self.aspect.y * g + 5.3, self.seed ^ 0x6a3);
        let keep = self.look.grain_floor + (1.0 - self.look.grain_floor) * slack;
        log_uniform(tilt, (self.look.grain * keep).max(1.0e-3))
    }
}
