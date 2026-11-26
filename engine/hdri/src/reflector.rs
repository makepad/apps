//! A flat collapsible reflector panel (the 100 x 140 cm five-in-one the
//! photographed plates RLS038-042 show): a white cloth, or a silver or gold
//! metallised one, sewn into a sprung frame that pulls it evenly all round
//! its rim, so no tension fans run in from pull points. What shows is the
//! cloth folded away and opened again, drawn by the owner's fabric recipe as
//! height lit through its normal, never as brightness: long straight hinge
//! creases at random angles (the panel twisted into three loops to pack),
//! broad flat facets between them (a smooth Voronoi of tilted planes, as an
//! open box's foil, open_box.rs), slack wrinkles along a slowly turning
//! direction, a fine crinkle in patches and broad soft folds, all calmer by
//! the frame where the cloth is taut.
//!
//! The light that lights the panel (the source) stands in front of it, on
//! its axis, `SOURCE_DISTANCE` half widths off, its height on the face from
//! `plate` (0.5 the middle). The white cloth is diffuse: its glow falls off
//! round the source's foot as the inverse square and the cosine do, and its
//! folds are lit on the side that faces the source. Silver and gold are a
//! mirror: each point shows the source where its normal turns the viewer's
//! ray onto it, so the source's image, wider than tall as the plates' are,
//! `baffle` setting its size, breaks into lit and dark facets and throws
//! glints far out. Gold is silver tinted (`finish_tint`, the light's colour
//! carries it, studio.rs). The binding is a dark band `rim` of the shorter
//! half size wide round the outline, a superellipse.
//!
//! Positions are box units in, x to the viewer's right and y up; the
//! pattern works in half widths (the shorter half size is 1). The face sits
//! at mean 1 over the outline (`norm`, sampled once per light). Per point:
//! three height evaluations (thirty creases, 25 facet cells, a few octaves
//! of value noise), a few microseconds; only a reflector pays it.
//!
//! A cloth pulled taut one way (`pull`, the round bounce cards of the area
//! maps, Bounce_Card_Silver and _Gold) shows its tension as well: long folds
//! along the pull (`PULL_*`), by the recipe's tension map. The frame pulls it
//! unevenly, at a few points on either side along the pull, each its own
//! strength and slightly turned; the fans' frames, distance along and angle
//! across, blend by strength over r^2. The folds are single folds laid at
//! hashed places in that frame, so their spacing, length and height vary,
//! coming in and going out along their length, a little skewed so they merge
//! and part, some pinched to a crisp crease, leaning to one side, the frame
//! warped low so they bend together; all lit through their normal as the
//! rest. The crumple's hinge creases, facets and slack wrinkles flatten where
//! the pull is strong but still show under the folds, its fine creases and
//! crinkle stay, and the source's image spreads along the folds
//! (`PullLook`): the light lies across the card. The silver card's folds
//! are shorter and bent, packed denser (`SILVER_PULL`), the gold's long and
//! straighter (`CARD_PULL`).
//! A round card (a disc light) draws its outline and binding on the circle.
use makepad_draw::*;

use super::noise::{fbm2, hash01};
use super::patterns::{broad_folds, patches};
use super::params::ModifierParams;

/// The plates' outline: |x|^n + |y|^n = 1 over the face's box (RLS038-042
/// fit n 2.8, the box 1.43 times as tall as wide, filling 0.87 of it).
const OUTLINE_POWER: f32 = 2.8;
/// The outline's soft edge, in half widths.
const OUTLINE_SOFT: f32 = 0.012;
/// The back of the cloth (`Reflector::back`): the cover's plain black
/// fabric's albedo, and the room's fill on it as a share of the face's mean
/// (light_model.rs's `ROOM_FILL`).
const BACK_ALBEDO: f32 = 0.05;
const BACK_FILL: f32 = 0.19;
/// The binding's level over the cloth's dull level beside it (the plates'
/// 0.05-0.14 of their medians), and how far in from it the cloth still
/// dips toward the frame's sleeve (RLS042: 0.75 just inside, 1.0 by 1.5 %
/// of the width).
const BINDING_LEVEL: f32 = 0.08;
const SLEEVE: f32 = 0.03;
/// How far in front of the panel the source stands, in half widths.
const SOURCE_DISTANCE: f32 = 2.7;
/// The white cloth's light that does not come from the source (the room's).
const WHITE_AMBIENT: f32 = 0.02;
/// The white cloth's satin gloss: its share of the light, and how much wider
/// than the source's image it spreads (RLS042's wrinkles catch it on their
/// sides as bright edges).
const WHITE_GLOSS: f32 = 0.12;
const WHITE_GLOSS_SPREAD: f32 = 2.5;
/// The mirror: the source's image's angular half size across (radians) and
/// its height over its width, at `baffle` 1, and at least `SOURCE_MIN` of
/// that at 0.
const SOURCE_ACROSS: f32 = 0.3;
const SOURCE_TALL: f32 = 0.5;
const SOURCE_MIN: f32 = 0.25;
/// The metallised cloth's dull part: the light it scatters, as a share of
/// the source's image's peak.
const SILVER_DULL: f32 = 0.012;
/// A silver panel's median over its dull level (its glints lift it): what
/// its binding's level is a share of.
const BINDING_SILVER: f32 = 2.0;
/// How far in from the frame the cloth is taut, in half widths.
const TAUT: f32 = 0.12;
/// Gold over silver, linear RGB, as the plates read it (RLS038 over
/// RLS040, the same in their hot and their dull texels).
pub const GOLD: [f32; 3] = [1.0, 0.610, 0.404];
/// A pulled cloth's tension frame (`pull`), in half widths. The frame does
/// not pull evenly along its edge: `PULL_POINTS` points on either side, each
/// `PULL_POINT` out along the pull, spread `PULL_SPREAD_ACROSS` across, with
/// its own strength (`PULL_WEAK` to 1) and its fan turned by up to
/// `PULL_TURN` radians. The frame is warped low (`PULL_BEND` half widths) and
/// at the folds' scale (`PULL_WARP` of `PULL_FOLD`, the spacing the strong
/// folds read at).
const PULL_FOLD: f32 = 0.14;
const PULL_POINT: f32 = 4.0;
const PULL_POINTS: i32 = 3;
const PULL_SPREAD_ACROSS: f32 = 0.8;
const PULL_WEAK: f32 = 0.3;
const PULL_TURN: f32 = 0.1;
const PULL_BEND: f32 = 0.35;
const PULL_WARP: f32 = 0.6;

/// How a pulled card's foil folds (`Reflector::folds`), in half widths: how
/// many single folds lie at hashed places in the tension frame (so their
/// spacing varies), their half lengths (`short`: most of them short, the
/// hash squared), widths (`slim`: the short ones up to that much
/// narrower), skew off the frame (across per along, so neighbours merge and
/// part), bow (across per along squared) and wave (its height across and
/// its frequency along) along their length; a fold's
/// height, log-uniform over `height_spread` either way, up to `pinch` of it
/// a pinched crease (its glint), one side up to `lean` steeper than the
/// other; how much of the crumple's hinge creases, facets and slack
/// wrinkles stays where the pull is full (`crumple`) and of the hinge
/// creases on top (`hinge`); and how much wider the source's image spreads
/// along the folds on a fully pulled card (the light lies across the whole
/// card) and across them.
#[derive(Clone, Copy, Debug)]
struct PullLook {
    folds: i32,
    length: (f32, f32),
    short: bool,
    width: (f32, f32),
    slim: f32,
    skew: f32,
    bow: f32,
    wave: (f32, f32),
    height: f32,
    height_spread: f32,
    pinch: f32,
    lean: f32,
    crumple: f32,
    hinge: f32,
    spread: f32,
    spread_cross: f32,
}

/// The gold card's foil (Bounce_Card_Gold, and a white card's cloth): long
/// folds, fairly straight, about 14 across.
const CARD_PULL: PullLook = PullLook {
    folds: 260,
    length: (0.2, 0.9),
    short: false,
    width: (0.025, 0.075),
    slim: 0.0,
    skew: 0.08,
    bow: 0.0,
    wave: (0.0, 0.0),
    height: 0.0095,
    height_spread: 1.5,
    pinch: 0.8,
    lean: 0.5,
    crumple: 0.6,
    hinge: 1.0,
    spread: 2.5,
    spread_cross: 0.4,
};

/// The silver card's foil (Bounce_Card_Silver): shorter folds, most of them
/// short, bowed and waving along their length, overlapping in a dense
/// crumpled pack about 18-20 across, the crumple showing more between them
/// and few long hinge creases; the image spreads less, so the middle stays
/// darker between the folds.
const SILVER_PULL: PullLook = PullLook {
    folds: 850,
    length: (0.1, 0.45),
    short: true,
    width: (0.02, 0.06),
    slim: 0.5,
    skew: 0.2,
    bow: 1.0,
    wave: (0.025, 6.0),
    height: 0.0095,
    height_spread: 1.5,
    pinch: 0.6,
    lean: 0.5,
    crumple: 0.85,
    hinge: 0.35,
    spread: 1.0,
    spread_cross: 0.25,
};

/// How one finish's cloth is folded, in half widths: the hinge creases (how
/// many, their bend each side as a slope, the crisp width at the fold, how
/// far the bend flattens out to either side), the fine creases of the
/// crumple (one to a cell of `fine_cell` half widths, as long as a cell or
/// two, bending by `fine_bend`), the facets (cells per half
/// width, their tilt as a slope, how softly neighbours blend as a share of
/// a cell), the slack wrinkles (ridges along a slowly turning direction:
/// their height, frequency along and across), the crinkle (frequency and
/// height, in patches) and the broad soft folds (frequency and height).
#[derive(Clone, Copy, Debug)]
struct Look {
    creases: i32,
    crease_length: (f32, f32),
    crease_wiggle: f32,
    crease_bend: f32,
    crease_soft: f32,
    crease_reach: f32,
    fine_cell: f32,
    fine_bend: f32,
    facet_freq: f32,
    facet_tilt: f32,
    facet_soft: f32,
    wrinkle_amp: f32,
    wrinkle_along: f32,
    wrinkle_across: f32,
    crinkle_freq: f32,
    crinkle_amp: f32,
    crinkle_floor: f32,
    broad_freq: f32,
    broad_amp: f32,
}

/// The white cloth (RLS042): soft, its creases and wrinkles broad.
const WHITE_LOOK: Look = Look {
    creases: 110,
    crease_length: (0.08, 0.45),
    crease_wiggle: 0.006,
    crease_bend: 0.1,
    crease_soft: 0.006,
    crease_reach: 0.12,
    fine_cell: 0.05,
    fine_bend: 0.2,
    facet_freq: 5.0,
    facet_tilt: 0.025,
    facet_soft: 0.25,
    wrinkle_amp: 0.0,
    wrinkle_along: 2.5,
    wrinkle_across: 11.0,
    crinkle_freq: 30.0,
    crinkle_amp: 0.002,
    crinkle_floor: 0.0,
    broad_freq: 1.4,
    broad_amp: 0.02,
};

/// The metallised cloth (RLS038-041): stiffer, so its creases stay crisp
/// and its crinkle fine, and a mirror shows every tilt twice over, so its
/// slopes are smaller.
const FOIL_LOOK: Look = Look {
    creases: 90,
    crease_length: (0.08, 0.5),
    crease_wiggle: 0.01,
    crease_bend: 0.18,
    crease_soft: 0.002,
    crease_reach: 0.03,
    fine_cell: 0.05,
    fine_bend: 0.12,
    facet_freq: 10.0,
    facet_tilt: 0.02,
    facet_soft: 0.12,
    wrinkle_amp: 0.0012,
    wrinkle_along: 3.0,
    wrinkle_across: 14.0,
    crinkle_freq: 35.0,
    crinkle_amp: 0.0012,
    crinkle_floor: 0.5,
    broad_freq: 1.4,
    broad_amp: 0.004,
};

/// The panel's finish (`ModifierParams::ribs`: 0 white, 1 silver, 2 gold).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finish {
    White,
    Silver,
    Gold,
}

impl Finish {
    pub fn from_ribs(ribs: u32) -> Finish {
        match ribs {
            0 => Finish::White,
            1 => Finish::Silver,
            _ => Finish::Gold,
        }
    }
}

/// The colour a finish puts on its light, at luminance 1: gold's tint, else white.
pub fn finish_tint(finish: Finish) -> Vec3f {
    match finish {
        Finish::Gold => {
            let g = vec3f(GOLD[0], GOLD[1], GOLD[2]);
            g / (0.2126 * g.x + 0.7152 * g.y + 0.0722 * g.z)
        }
        _ => vec3f(1.0, 1.0, 1.0),
    }
}

use makepad_draw::smoothstep as smooth;

/// One of the points the frame pulls a card's cloth at (`Reflector::pulls`):
/// out `along` the pull, `across` it, its strength and its fan's turn.
#[derive(Clone, Copy, Debug)]
struct PullPoint {
    along: f32,
    across: f32,
    strength: f32,
    turn: f32,
}

/// One tension fold of a pulled cloth (`Reflector::folds`), in the tension
/// frame: its middle along and across, its half length, width, skew off the
/// frame (across per along), where along it peaks (`peak`, from the middle),
/// its bow (across per along squared), its wave (height across, frequency
/// along, phase), its height (signed: a ridge or a groove), its share of
/// pinched crease and its lean (one side steeper than the other, as a fold
/// lies over).
#[derive(Clone, Copy, Debug)]
struct Fold {
    along: f32,
    across: f32,
    half_length: f32,
    width: f32,
    skew: f32,
    bow: f32,
    wave: (f32, f32, f32),
    peak: f32,
    height: f32,
    pinched: f32,
    lean: f32,
}

#[derive(Clone, Debug)]
pub struct Reflector {
    finish: Finish,
    look: Look,
    /// The outline's power: 2.8 the panels' superellipse, 2 a round card.
    power: f32,
    /// How taut the cloth is pulled (0 crumpled), and the pull's way (unit,
    /// half widths: x right, y up).
    pull: f32,
    pull_dir: Vec2f,
    /// How its foil folds when pulled, the frame's pull points and the
    /// tension folds (empty unpulled).
    pull_look: PullLook,
    pulls: Vec<PullPoint>,
    folds: Vec<Fold>,
    /// The box's half sizes in half widths (1 on the shorter side).
    aspect: Vec2f,
    rim: f32,
    /// The source, in half widths in front of the face's middle.
    source: Vec3f,
    /// The source's image's angular half sizes, across and along (along and
    /// across the pull on a pulled cloth).
    spread: Vec2f,
    seed: u32,
    norm: f32,
}

impl Reflector {
    /// A panel on a face of tangent half extents `half`, its binding `rim`
    /// of the shorter half size, its source's size `baffle` and its height
    /// on the face `plate` (0 the top, 0.5 the middle, 1 the bottom) and
    /// across it `across` (-1 the left edge, 0 the middle, 1 the right; RLS042's
    /// glow sits right of the middle), the finish from `ribs`, how taut its
    /// cloth is pulled `pull` and which way `pull_angle` (degrees from x,
    /// counter-clockwise); `round` a round card (a disc light).
    pub fn new(half: Vec2f, p: &ModifierParams, round: bool, seed: u32) -> Reflector {
        let (rim, baffle, plate, across) = (p.rim, p.baffle, p.plate, p.across.unwrap_or(0.0));
        let short = half.x.min(half.y).max(1.0e-6);
        let aspect = vec2f(half.x / short, half.y / short);
        let size = SOURCE_MIN + (1.0 - SOURCE_MIN) * baffle.clamp(0.0, 1.0);
        let finish = Finish::from_ribs(p.ribs);
        let pull = p.pull.unwrap_or(0.0).clamp(0.0, 1.0);
        let (sa, ca) = p.pull_angle.unwrap_or(0.0).to_radians().sin_cos();
        let pull_look = if finish == Finish::Silver { SILVER_PULL } else { CARD_PULL };
        let mut spread = vec2f(SOURCE_ACROSS * size, SOURCE_ACROSS * SOURCE_TALL * size);
        if pull > 0.0 {
            // The image spreads along the folds: its half sizes in the pull's frame (the
            // ellipse's radii along and across the pull), the one along the pull grown.
            let radius = |c: f32, s: f32| 1.0 / ((c / spread.x).powi(2) + (s / spread.y).powi(2)).sqrt();
            spread = vec2f(radius(ca, sa) * (1.0 + pull_look.spread * pull), radius(-sa, ca) * (1.0 + pull_look.spread_cross * pull));
        }
        let mut r = Reflector {
            finish,
            look: if finish == Finish::White { WHITE_LOOK } else { FOIL_LOOK },
            power: if round { 2.0 } else { OUTLINE_POWER },
            pull,
            pull_dir: vec2f(ca, sa),
            pull_look,
            pulls: Vec::new(),
            folds: Vec::new(),
            aspect,
            rim,
            source: vec3f(across.max(-1.0).min(1.0) * aspect.x, (1.0 - 2.0 * plate.clamp(0.0, 1.0)) * aspect.y, SOURCE_DISTANCE),
            spread,
            seed,
            norm: 1.0,
        };
        if pull > 0.0 {
            r.lay_folds();
        }
        // The mean over the outline, a 96 x 96 midpoint grid over the box.
        let n = 96;
        let (mut sum, mut count) = (0.0f64, 0u32);
        for j in 0..n {
            for i in 0..n {
                let u = vec2f(2.0 * (i as f32 + 0.5) / n as f32 - 1.0, 2.0 * (j as f32 + 0.5) / n as f32 - 1.0);
                if r.outline(u).0 > 0.5 {
                    sum += r.raw(u) as f64;
                    count += 1;
                }
            }
        }
        r.norm = ((sum / count.max(1) as f64) as f32).max(1.0e-4);
        r
    }

    pub fn finish(&self) -> Finish {
        self.finish
    }

    /// The outline's power: |x|^n + |y|^n = 1 over the box (2.8 a panel, 2 a
    /// round card), as the 3D model draws its hoop.
    pub fn outline_power(&self) -> f32 {
        self.power
    }

    pub fn factor(&self, u: Vec2f) -> f32 {
        self.raw(u) / self.norm
    }

    /// Inside the outline (1) or out (0), and how far in, in half widths.
    fn outline(&self, u: Vec2f) -> (f32, f32) {
        let n = self.power;
        let (ax, ay) = (u.x.abs().max(1.0e-6), u.y.abs().max(1.0e-6));
        let s = (ax.powf(n) + ay.powf(n)).powf(1.0 / n);
        // The gradient of s in half widths, to turn 1 - s into a distance.
        let k = s.powf(1.0 - n);
        let gx = ax.powf(n - 1.0) * k / self.aspect.x;
        let gy = ay.powf(n - 1.0) * k / self.aspect.y;
        let d = (1.0 - s) / gx.hypot(gy).max(1.0e-4);
        (smooth(-OUTLINE_SOFT, OUTLINE_SOFT, d), d)
    }

    /// The cloth's height at `p` (half widths), `taut` 0 at the frame to 1
    /// inside: hinge creases, facets, slack wrinkles, crinkle and broad folds.
    fn height(&self, p: Vec2f, taut: f32) -> f32 {
        let (seed, look) = (self.seed, &self.look);
        let mut h = 0.0f32;
        // A pulled cloth's crumple flattens (its hinge creases, facets and
        // slack wrinkles; the pull look's `crumple` of them stay, `hinge` more
        // of the hinge creases), its tension folds come in.
        let tension = if self.pull > 0.0 { Some(self.tension_frame(p)) } else { None };
        let crumple = match tension {
            Some((_, _, strength)) => 1.0 - (1.0 - self.pull_look.crumple) * self.pull * strength,
            None => 1.0,
        };
        let hinge = if tension.is_some() { crumple * self.pull_look.hinge } else { crumple };
        // The hinge creases: straight lines through hashed points at hashed
        // angles, each bending the cloth one way along a crisp fold that
        // flattens out to either side and fades at its ends.
        for i in 0..look.creases {
            let k = |j: i32| hash01(i, j, 11, seed);
            let centre = vec2f((2.0 * k(0) - 1.0) * 0.9 * self.aspect.x, (2.0 * k(1) - 1.0) * 0.9 * self.aspect.y);
            let (sa, ca) = (std::f32::consts::PI * k(2)).sin_cos();
            let (dx, dy) = (p.x - centre.x, p.y - centre.y);
            let along = dx * ca + dy * sa;
            let half_length = (look.crease_length.0 + (look.crease_length.1 - look.crease_length.0) * k(3)) * self.aspect.x;
            let window = 1.0 - smooth(0.5 * half_length, half_length, along.abs());
            if window <= 0.0 {
                continue;
            }
            let across = dy * ca - dx * sa + look.crease_wiggle * (2.0 * fbm2(along * 9.0 + k(7) * 50.0, 0.5, 2, seed.wrapping_add(3)) - 1.0);
            let bend = look.crease_bend * (0.4 + 0.8 * k(4)) * if k(5) < 0.5 { -1.0 } else { 1.0 };
            let reach = look.crease_reach * (0.6 + 0.8 * k(6));
            let flatten = (-(across / reach).powi(2)).exp();
            h += hinge * bend * ((across * across + look.crease_soft * look.crease_soft).sqrt() - look.crease_soft) * flatten * window;
        }
        // The fine creases of the crumple: one to a cell, through a hashed
        // point at a hashed angle, a cell or two long, the bend flattening
        // out within a third of a cell.
        let c = look.fine_cell;
        let (gx, gy) = (p.x / c, p.y / c);
        let (fx, fy) = (gx.floor() as i32, gy.floor() as i32);
        for j in -1..=1i32 {
            for i in -1..=1i32 {
                let (cx, cy) = (fx + i, fy + j);
                let k = |n: i32| hash01(cx, cy, n, seed.wrapping_add(41));
                let (sa, ca) = (std::f32::consts::PI * k(2)).sin_cos();
                let (dx, dy) = (gx - cx as f32 - k(0), gy - cy as f32 - k(1));
                let along = dx * ca + dy * sa;
                let half_length = 0.5 + 0.5 * k(3);
                let window = 1.0 - smooth(0.4 * half_length, half_length, along.abs());
                if window <= 0.0 {
                    continue;
                }
                let across = (dy * ca - dx * sa) * c;
                let bend = look.fine_bend * (0.3 + 0.9 * k(4)) * if k(5) < 0.5 { -1.0 } else { 1.0 };
                let reach = 0.3 * c;
                let soft = 0.15 * c;
                h += bend * ((across * across + soft * soft).sqrt() - soft) * (-(across / reach).powi(2)).exp() * window;
            }
        }
        // The facets: one plane per cell of a jittered lattice, each tilted
        // its own way, blended with its neighbours softly (a smooth Voronoi
        // of planes; five by five cells, as an open box's, so no plane that
        // still weighs drops at a cell's border).
        let f = look.facet_freq;
        let (x, y) = (p.x * f + 0.37, p.y * f + 2.9);
        let (ix, iy) = (x.floor(), y.floor());
        let mut planes = [(0.0f32, 0.0f32); 25];
        let mut nearest = f32::INFINITY;
        let mut n = 0;
        for j in -2..=2i32 {
            for i in -2..=2i32 {
                let (cx, cy) = (ix as i32 + i, iy as i32 + j);
                let dx = x - (ix + i as f32 + hash01(cx, cy, 0, seed ^ 0x3f1));
                let dy = y - (iy + j as f32 + hash01(cx, cy, 1, seed ^ 0x3f1));
                let d = dx.hypot(dy);
                let ga = 2.0 * hash01(cx, cy, 2, seed ^ 0x3f1) - 1.0;
                let gb = 2.0 * hash01(cx, cy, 3, seed ^ 0x3f1) - 1.0;
                planes[n] = (d, (ga * dx + gb * dy) / f);
                n += 1;
                nearest = nearest.min(d);
            }
        }
        let floor = (-0.9 / look.facet_soft).exp();
        let (mut sum, mut weights) = (0.0f32, 0.0f32);
        for (d, plane) in planes {
            let w = ((-(d - nearest) / look.facet_soft).exp() - floor).max(0.0);
            sum += w * plane;
            weights += w;
        }
        h += crumple * look.facet_tilt * sum / weights.max(1.0e-9);
        // The slack wrinkles: ridges along a direction that turns slowly
        // over the panel (where the twist left the cloth slack), squeezed
        // across, domain-warped, between a pinched ridge and a soft fold, in
        // patches.
        let turn = std::f32::consts::TAU * fbm2(p.x * 0.5 + 8.8, p.y * 0.5 + 3.4, 2, seed.wrapping_add(19));
        let (st, ct) = turn.sin_cos();
        let (a, b) = ((p.x * ct + p.y * st) * look.wrinkle_along, (p.y * ct - p.x * st) * look.wrinkle_across);
        let wa = fbm2(a * 0.5 + 1.3, b * 0.2 + 7.1, 2, seed.wrapping_add(23)) - 0.5;
        let v = ((fbm2(a + 1.5 * wa, b + 1.5 * wa, 3, seed.wrapping_add(29)) - 0.5) * 1.6 + 0.5).clamp(0.0, 1.0);
        let wpatch = smooth(0.3, 0.6, fbm2(p.x * 0.9 + 2.2, p.y * 0.9 + 6.6, 2, seed.wrapping_add(31)));
        h += crumple * look.wrinkle_amp * wpatch * (0.4 * (1.0 - (2.0 * v - 1.0).abs()) + 0.6 * v);
        if let Some(frame) = tension {
            h += self.pull * self.tension(frame);
        }
        // The crinkle, in patches, and the broad soft folds.
        let patch = look.crinkle_floor + (1.0 - look.crinkle_floor) * patches(p.x, p.y, 1.3, 0.4, 0.7, 2, seed.wrapping_add(7));
        h += look.crinkle_amp * patch * (fbm2(p.x * look.crinkle_freq + 5.1, p.y * look.crinkle_freq + 2.3, 2, seed.wrapping_add(9)) - 0.5);
        h += look.broad_amp * broad_folds(p.x * look.broad_freq, p.y * look.broad_freq, 0.6, seed.wrapping_add(13), seed.wrapping_add(17));
        h * taut
    }

    /// A pulled cloth's pull points and tension folds, hashed once: the
    /// frame pulls at `PULL_POINTS` points on either side, `PULL_POINT` out
    /// along the pull, spread across, each its own strength and turn; the
    /// folds lie at hashed places over the card in the tension frame (so
    /// their spacing varies), each its own length, width, skew, height and
    /// crease.
    fn lay_folds(&mut self) {
        let seed = self.seed;
        for (side, key) in [(-1.0f32, 81), (1.0, 83)] {
            for i in 0..PULL_POINTS {
                let k = |j: i32| hash01(i, j, key, seed);
                self.pulls.push(PullPoint {
                    along: side * PULL_POINT,
                    across: PULL_SPREAD_ACROSS * (2.0 * (i as f32 + k(0)) / PULL_POINTS as f32 - 1.0),
                    strength: PULL_WEAK + (1.0 - PULL_WEAK) * k(1),
                    turn: PULL_TURN * (2.0 * k(2) - 1.0),
                });
            }
        }
        // The turns even out over the frame, so the folds run along the pull on the whole.
        let (turns, strengths) = self.pulls.iter().fold((0.0, 0.0), |(t, w), q| (t + q.turn * q.strength, w + q.strength));
        for q in &mut self.pulls {
            q.turn -= turns / strengths;
        }
        let (look, reach) = (self.pull_look, self.aspect.x.max(self.aspect.y) * 1.15);
        for i in 0..look.folds {
            let k = |j: i32| hash01(i, j, 87, seed);
            let long = if look.short { k(2) * k(2) } else { k(2) };
            let half_length = look.length.0 + (look.length.1 - look.length.0) * long;
            self.folds.push(Fold {
                along: reach * (2.0 * k(1) - 1.0),
                across: reach * (2.0 * k(0) - 1.0),
                half_length,
                width: (look.width.0 + (look.width.1 - look.width.0) * k(3)) * (1.0 - look.slim * (1.0 - long)),
                skew: look.skew * (2.0 * k(4) - 1.0),
                bow: look.bow * (2.0 * k(10) - 1.0),
                wave: (look.wave.0 * k(11), look.wave.1 * (0.6 + 0.8 * k(12)), std::f32::consts::TAU * k(13)),
                peak: 0.6 * half_length * (2.0 * k(5) - 1.0),
                height: look.height * look.height_spread.powf(2.0 * k(6) - 1.0) * if k(7) < 0.35 { -1.0 } else { 1.0 },
                pinched: look.pinch * k(8) * k(8),
                lean: look.lean * (2.0 * k(9) - 1.0),
            });
        }
    }

    /// The tension map at `p` (half widths): each pull point is a fan whose
    /// frame runs along the distance from it and across its angle (as arc at
    /// the middle, turned its own way), the fans blended by their strength
    /// over r^2. Returns the point along the pull, across it in the blended
    /// frame, and the pull's strength there (the blended strengths, 1 the
    /// strongest point).
    fn tension_frame(&self, p: Vec2f) -> (f32, f32, f32) {
        let e = self.pull_dir;
        let (x, y) = (p.x * e.x + p.y * e.y, p.y * e.x - p.x * e.y);
        let (mut across, mut strength, mut weights) = (0.0f32, 0.0f32, 0.0f32);
        for q in &self.pulls {
            let (dx, dy) = ((x - q.along).abs(), y - q.across);
            let w = q.strength / (dx * dx + dy * dy).max(1.0e-4);
            across += w * ((dy.atan2(dx) + q.turn) * PULL_POINT + q.across);
            strength += w * q.strength;
            weights += w;
        }
        (x, across / weights, strength / weights)
    }

    /// A pulled cloth's tension folds in the tension map's frame (`along`,
    /// `across`, `strength`, from `tension_frame`): the folds, each coming
    /// in and going out along its length, turned a little off the frame,
    /// bowed and waving, between a pinched crease and a soft fold, raised or
    /// sunk, stronger where the pull is strong and in slowly wandering
    /// patches; the frame
    /// warped low so the folds bend together, and at their own scale so
    /// they wiggle.
    fn tension(&self, (x, across, strength): (f32, f32, f32)) -> f32 {
        let seed = self.seed;
        let bend = PULL_BEND * 2.0 * (fbm2(x * 0.45 + 3.7, across * 0.45 + 9.1, 2, seed.wrapping_add(53)) - 0.5);
        let a = across + bend;
        let a = a + PULL_WARP * PULL_FOLD * 2.0 * (fbm2(x * 1.4 + 6.1, a * 2.5 + 1.7, 2, seed.wrapping_add(59)) - 0.5);
        let mut h = 0.0f32;
        for f in &self.folds {
            let t = x - f.along;
            if t.abs() >= f.half_length {
                continue;
            }
            // Bowed and waving along its length (none on the gold card's straight folds).
            let d = a - f.across - f.skew * t - f.bow * t * t - f.wave.0 * (f.wave.1 * t + f.wave.2).sin();
            if d.abs() > 3.0 * f.width {
                continue;
            }
            // In and out along its length, peaking off its middle.
            let window = if t < f.peak { smooth(-f.half_length, f.peak, t) } else { 1.0 - smooth(f.peak, f.half_length, t) };
            let d = d / (1.0 + f.lean * d.signum());
            let soft = (-(d / f.width).powi(2)).exp();
            let pinch = (-(d * d + 1.0e-6).sqrt() / (0.5 * f.width)).exp();
            h += f.height * window * (soft + f.pinched * (pinch - soft));
        }
        let patch = 0.5 + 0.7 * smooth(0.3, 0.7, fbm2(x * 0.8 + 1.3, a * 1.4 + 5.9, 2, seed.wrapping_add(61)));
        h * patch * strength
    }

    /// The panel before its mean: the cloth lit by the source inside the
    /// outline, the binding round it, nothing outside.
    fn raw(&self, u: Vec2f) -> f32 {
        self.raw_seen(u, None).0
    }

    /// The panel at `u` seen along `view` (unit, toward the panel, its own
    /// frame: x right, y up, z out of its front) over the panel there seen
    /// head-on, from far off along -z (`raw`): a light's 3D model moves a
    /// metallised cloth's hot zone and a white one's gloss with the view
    /// (light_model_panel.rs `Sheen`). 1 outside the outline.
    pub fn seen(&self, u: Vec2f, view: Vec3f) -> f32 {
        let (head_on, seen) = self.raw_seen(u, Some(view));
        if head_on > 0.0 {
            seen / head_on
        } else {
            1.0
        }
    }

    /// The back of the cloth at `u`: the cover's plain black fabric, the
    /// front's folds (its height, mirrored) lit by the room's fill from
    /// above, as a share of the face's mean (`BACK_ALBEDO` of `BACK_FILL`);
    /// the binding round it as dark. 0 outside the outline.
    pub fn back(&self, u: Vec2f) -> f32 {
        let (inside, d) = self.outline(u);
        if inside <= 0.0 {
            return 0.0;
        }
        let p = vec2f(u.x * self.aspect.x, u.y * self.aspect.y);
        let taut = smooth(0.0, TAUT, d);
        let e = 0.003;
        let h0 = self.height(p, taut);
        let hx = (self.height(p + vec2f(e, 0.0), taut) - h0) / e;
        let hy = (self.height(p + vec2f(0.0, e), taut) - h0) / e;
        // The back's normal faces -z; the fill comes from above and a little behind.
        let normal = vec3f(hx, hy, -1.0).normalize();
        let fill = vec3f(0.0, 0.6, -0.8);
        inside * BACK_ALBEDO * BACK_FILL * (0.55 + 0.45 * normal.dot(fill).max(0.0))
    }

    /// The panel before its mean seen head-on (`raw`) and, with `view`, seen
    /// along it: the folded normal is the same for both.
    fn raw_seen(&self, u: Vec2f, view: Option<Vec3f>) -> (f32, f32) {
        let (inside, d) = self.outline(u);
        if inside <= 0.0 {
            return (0.0, 0.0);
        }
        let p = vec2f(u.x * self.aspect.x, u.y * self.aspect.y);
        let taut = smooth(0.0, TAUT, d);
        let e = 0.003;
        let h0 = self.height(p, taut);
        let hx = (self.height(p + vec2f(e, 0.0), taut) - h0) / e;
        let hy = (self.height(p + vec2f(0.0, e), taut) - h0) / e;
        let normal = vec3f(-hx, -hy, 1.0).normalize();
        let to = self.source - vec3f(p.x, p.y, 0.0);
        let r = to.length().max(1.0e-4);
        let l = to / r;
        // The source's light on the cloth, 1 at its foot on a flat panel.
        let falloff = SOURCE_DISTANCE * SOURCE_DISTANCE / (r * r);
        // The cloth's dull level there, flat: what the binding is a share of.
        let dull = match self.finish {
            Finish::White => WHITE_AMBIENT + (1.0 - WHITE_AMBIENT) * l.z * falloff,
            Finish::Silver | Finish::Gold => BINDING_SILVER * SILVER_DULL * falloff,
        };
        // The binding round the outline, and the sleeve's dip just inside it.
        let band = self.rim * self.aspect.x.min(self.aspect.y);
        let binding = smooth(band, band + 0.004, d);
        let sleeve = 1.0 - 0.25 * (1.0 - smooth(band, band + SLEEVE, d));
        let shade = |ray: Vec3f| {
            // The cloth shows the source as far as the turned ray falls
            // within the source's image (q, in its half sizes; wider than tall).
            let off = ray - l;
            let q = if self.pull > 0.0 {
                // A pulled cloth's image lies in the pull's frame.
                let e = self.pull_dir;
                ((off.x * e.x + off.y * e.y) / self.spread.x).hypot((off.y * e.x - off.x * e.y) / self.spread.y)
            } else {
                (off.x / self.spread.x).hypot(off.y / self.spread.y)
            };
            let lit = match self.finish {
                // Diffuse, and the satin weave's broad gloss (`WHITE_GLOSS`).
                Finish::White => {
                    let gloss = (-(q / WHITE_GLOSS_SPREAD).powi(2)).exp();
                    WHITE_AMBIENT + (1.0 - WHITE_AMBIENT - WHITE_GLOSS) * normal.dot(l).max(0.0) * falloff + WHITE_GLOSS * gloss * falloff
                }
                // A mirror: the source's image as a core and a softer skirt
                // round it, over the little the foil scatters.
                Finish::Silver | Finish::Gold => {
                    let image = 0.7 * (-q * q).exp() + 0.3 * (-1.5 * q).exp();
                    SILVER_DULL * (0.3 + 0.7 * normal.dot(l).max(0.0)) * falloff + image * falloff
                }
            };
            inside * (BINDING_LEVEL * dull + (lit * sleeve - BINDING_LEVEL * dull) * binding)
        };
        // The viewer looks along -z from far off; the cloth turns its ray to
        // 2 (n.z) n - z.
        let head_on = shade(normal * (2.0 * normal.z) - vec3f(0.0, 0.0, 1.0));
        let seen = match view {
            Some(v) => shade(v - normal * (2.0 * v.dot(normal))),
            None => head_on,
        };
        (head_on, seen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel(f: impl FnOnce(&mut ModifierParams), round: bool) -> Reflector {
        let mut p = ModifierParams { kind: "reflector".to_string(), rim: 0.016, baffle: 0.5, ribs: 1, plate: 0.5, ..ModifierParams::default() };
        f(&mut p);
        Reflector::new(vec2f(0.27, 0.27), &p, round, 7)
    }

    #[test]
    fn an_unpulled_cloth_draws_as_before() {
        // Pull 0 written out is the crumpled cloth exactly (RLS038-042 keep their look).
        let (a, b) = (panel(|_| {}, false), panel(|p| p.pull = Some(0.0), false));
        for k in 0..400 {
            let u = vec2f(2.0 * ((k % 20) as f32 + 0.5) / 20.0 - 1.0, 2.0 * ((k / 20) as f32 + 0.5) / 20.0 - 1.0);
            assert_eq!(a.factor(u).to_bits(), b.factor(u).to_bits());
        }
    }

    #[test]
    fn a_round_card_keeps_its_binding_on_the_circle() {
        // A squircle's corner (inside it, outside the circle) is outside a round card, and its binding sits just
        // inside the circle, where a disc light cuts the face.
        let (square, round) = (panel(|_| {}, false), panel(|_| {}, true));
        let corner = vec2f(0.76, 0.76);
        assert!(square.factor(corner) > 0.05 && round.factor(corner) == 0.0);
        let rim = |r: &Reflector, at: f32| r.factor(vec2f(at, 0.0));
        assert!(rim(&round, 0.995) < 0.3 * rim(&round, 0.9), "binding {} cloth {}", rim(&round, 0.995), rim(&round, 0.9));
        assert_eq!(round.outline_power(), 2.0);
    }

    #[test]
    fn a_pulled_cloths_folds_run_along_the_pull() {
        // The face changes faster across the pull than along it (Bounce_Card_Silver: long folds at 11 degrees),
        // though the crumple still shows under them, as on the plate.
        for angle in [11.0f32, -18.0, 90.0] {
            let r = panel(|p| {
                p.pull = Some(1.0);
                p.pull_angle = Some(angle);
            }, true);
            let (s, c) = angle.to_radians().sin_cos();
            let (mut along, mut across) = (0.0f32, 0.0f32);
            for k in 0..900 {
                let u = vec2f(0.6 * (2.0 * ((k % 30) as f32 + 0.5) / 30.0 - 1.0), 0.6 * (2.0 * ((k / 30) as f32 + 0.5) / 30.0 - 1.0));
                let d = 0.01;
                along += (r.factor(u + vec2f(c, s) * d) - r.factor(u)).abs();
                across += (r.factor(u + vec2f(-s, c) * d) - r.factor(u)).abs();
            }
            assert!(across > 1.5 * along, "{angle}: across {across} along {along}");
        }
    }
}
