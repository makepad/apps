//! A deep silver umbrella's face, close up: the flash's reflection seen in a
//! crumpled silver bowl, with the strobe head and its stand in front.
//!
//! The reflection is a near-round clipped core (reaching as far up as
//! sideways, the upper half a little further) that breaks into one lobe per
//! panel past about 0.4 of the radius. The lobe's edge is cut by short crease
//! folds anchored on the ribs and slanting back toward the hub: each a crisp
//! bright bar, brightest at its lip and pointed at its far end, with a hard
//! dark crease on its rim side. The folds push the reflection outward (glints
//! past the cliff) and shade the dull fabric. The panels (gores) are curved: a
//! dark trough beside each rib, a sheen band beside the rib on the side that
//! faces up, a light-to-dark sweep across the gore. The outer fabric keeps
//! wavy wrinkle bands, crinkle glints, sparkle, a woven foil mesh and broad
//! wrinkle shading out to a thin light bound edge at the rim. In front sit the
//! strobe head (front ring, rim highlight, display, button), the yoke with its
//! tilt knob, the spigot, the clamp with its lever and knob, and the pole.
//!
//! The outline is a polygon with its corners on the ribs (16 by default), a
//! panel's middle straight up and one straight down, where the pole runs.
//! Positions are box units, x to the viewer's right and y up, the ribs' ends
//! at radius 1.
//!
//! This is the owner's chosen prototype (the g2 lane of the studio lights'
//! face measurements) ported point by point. The prototype rendered on a
//! grid and evaluated its slow fields on a coarser one; here every field is
//! evaluated at the point, and the bake's anti-aliasing does the
//! supersampling. Its value noise was smoothstep-interpolated; value2 uses
//! the quintic fade, whose values spread about 5 % wider, so the noise here
//! is pulled toward its middle by `NOISE_MATCH` to keep the streaks', glints'
//! and scratches' contrast and counts.
//!
//! The fold shapes are tabled per rib side and cell in `DeepSilver::new`, and
//! each point only visits the five cells nearest it in each of the two fold
//! sizes. The face's level is sampled once per light, there too. Per point:
//! about forty value-noise lookups, ten fold cells and the stand's shapes, a
//! few microseconds; only a deep silver umbrella pays it.
use makepad_draw::*;

use super::modifier::log_uniform;
use super::noise::{fbm2, hash01, value2};

const DEG: f32 = std::f32::consts::PI / 180.0;

/// The prototype's value noise spread over value2's (the standard deviations
/// of smoothstep- and quintic-interpolated value noise, measured).
const NOISE_MATCH: f32 = 0.948;

/// Samples per axis of the level table (`DeepSilver::new`): 48 x 48 puts
/// the face's mean within about 2 % of a fine evaluation and costs a few
/// milliseconds each time a bake builds the light.
const TABLE_SAMPLES: usize = 48;

/// Long tent creases per panel.
const CREASES: usize = 3;

/// How the face is drawn. Lengths are box units (the ribs' ends at 1),
/// angles degrees.
#[derive(Clone, Copy, Debug)]
struct Look {
    /// The lobe's edge: its radius, how much further it reaches up, its slant
    /// across a panel, how much sooner it ends beside the ribs, its ragged
    /// jitter and fine streaks, and its softness.
    e0: f32,
    e_up: f32,
    e_slant: f32,
    e_rib: f32,
    e_jit: f32,
    e_fstr: f32,
    edge_w: f32,
    /// The tail past the edge (level and reach), the core's fall toward the
    /// hub, and the shade the strobe's body casts on the gores straight below.
    tail_a: f32,
    tail_w: f32,
    hub_fall: f32,
    hshade: f32,
    /// The clipped core: its level over the dull fabric, the floor across a
    /// panel, the core's width across a panel at the hub and further out, how
    /// far it gathers toward the panel's lower side, the streaks along the
    /// panels (broad and fine, and the fine ones' count per panel) and each
    /// lobe's own level.
    plateau: f32,
    cmin: f32,
    sw_hub: f32,
    core_shift: f32,
    sw0: f32,
    streak: f32,
    streak_fine: f32,
    fine_n: f32,
    lobe_var: f32,
    /// The long tent creases near the radius: their slope, and what it does
    /// to the reflection, the dull fabric and the core's width.
    crease_a: f32,
    facet_spec: f32,
    facet_diff: f32,
    tilt_k: f32,
    /// The crease folds on the ribs: their strength (large and small), the
    /// share of cells holding one, their slant range, the dark crease's width
    /// (in bar widths) and how dim a fold is at its rib.
    fold_big: f32,
    fold_small: f32,
    keep_big: f32,
    keep_small: f32,
    al0: f32,
    al1: f32,
    crease_w: f32,
    rib_dim: f32,
    /// The large folds: how they gather in runs along a rib, their cell,
    /// length and width ranges, the share that start a little off the rib,
    /// how pointed their far end is, how far toward the panel's middle they
    /// reach, the weak side's strength, the hard edge's width, the bar's soft
    /// share, the fade along a fold and its softly lit flank.
    clus_f: f32,
    cell_big: f32,
    len0: f32,
    len1: f32,
    wid0: f32,
    wid1: f32,
    off_p: f32,
    taper: f32,
    reach: f32,
    weak_side: f32,
    edge_px: f32,
    soft: f32,
    fade_u: f32,
    body: f32,
    /// What the folds do: brighten the core and darken it at their creases
    /// (in the core, at the cliff), push the reflection out, light and shade
    /// the dull fabric, glint past the cliff, tilt the core away.
    g_core: f32,
    k_core: f32,
    k_cliff: f32,
    disp: f32,
    k_disp: f32,
    g_out: f32,
    glint_far: f32,
    far_w: f32,
    k_out: f32,
    tilt_f: f32,
    /// The dull fabric: its level, its rise toward the rim, a panel's lighter
    /// middle, the tilt toward the side facing up, each panel's own shade,
    /// the folds' crease shading, the trough beside a rib (depth and width),
    /// the sheen band (level, place and width) and the broad wrinkle shading.
    floor: f32,
    out_rise: f32,
    outer_mid: f32,
    tilt: f32,
    panel_var: f32,
    crease: f32,
    trough: f32,
    trough_w: f32,
    sheen: f32,
    sheen_d: f32,
    sheen_w: f32,
    broad: f32,
    /// The ribs: their darkness in the core and outside it, their width, the
    /// lit edge on the side facing up, and the knot half way out.
    rib_in: f32,
    rib_out: f32,
    rib_w: f32,
    rib_lift: f32,
    knot: f32,
    /// The outer zone: the seam down a panel's middle, the wrinkle bands
    /// (strength and pitch), the rumples, the rim's dimming, the hem and the
    /// bound edge inside it.
    seam: f32,
    wrinkle: f32,
    wr_pitch: f32,
    rumple: f32,
    rim_dim: f32,
    hem: f32,
    bound: f32,
    /// The fine fabric: the woven mesh (in the core, outside it, its pitch),
    /// the crinkle (in, out), the pleats round the hub, the crinkle glints
    /// (strength and threshold), the sparkle (strength and share of cells)
    /// and the scratches (strength and threshold).
    weave_in: f32,
    weave_out: f32,
    weave_pitch: f32,
    crinkle_in: f32,
    crinkle_out: f32,
    pleat: f32,
    cglint: f32,
    cg_t: f32,
    sparkle: f32,
    spark_p: f32,
    scratch: f32,
    scr_t: f32,
    /// How big the fabric's features are on the face: the folds, ribs,
    /// troughs, seams, wrinkles, rumples, weave, crinkle, sparkle and
    /// scratches keep their size in the cloth, so on a smaller umbrella they
    /// take a larger share of its radius (1 for the deep 165 cm bowl).
    size: f32,
    /// Where the seam down a panel's middle starts, over the radius, and
    /// its width.
    seam_from: f32,
    seam_w: f32,
    /// The foil's grain: one flat facet per grain (a grain 0.0064 of the
    /// radius at `size` 1), each mirroring the light at its own tilt
    /// (log-uniform over a factor e^`grain`; 0 none).
    grain: f32,
    /// The seam down a panel's middle as the cloth's height, from the
    /// panel's tension map (`DeepSilver::seam_height`): its puckers' height
    /// where the ribs pull across it, the sewn ridge's height, and how
    /// strongly the flash picks out their slopes (0, 0: the painted `seam`
    /// line alone).
    pucker: f32,
    seam_ridge: f32,
    seam_light: f32,
    /// The seam's stitched fold: where the two panels are sewn the cloth
    /// turns under into a narrow steep groove down the seam. On the mirror
    /// foil any tilt turns the flash's reflection away, so both its walls
    /// darken by their slope, wherever the flash is: RLS025's seams read as
    /// thin dark lines right into the hub. How strongly (0 none).
    stitch: f32,
}

const DEEP: Look = Look {
    e0: 0.40,
    e_up: 0.035,
    e_slant: 0.04,
    e_rib: 0.035,
    e_jit: 0.12,
    e_fstr: 0.015,
    edge_w: 0.02,
    tail_a: 0.09,
    tail_w: 0.06,
    hub_fall: 0.15,
    hshade: 0.75,
    plateau: 220.0,
    cmin: 0.15,
    sw_hub: 0.75,
    core_shift: 0.25,
    sw0: 0.65,
    streak: 2.6,
    streak_fine: 0.9,
    fine_n: 14.0,
    lobe_var: 0.2,
    crease_a: 0.25,
    facet_spec: 0.4,
    facet_diff: 0.15,
    tilt_k: 0.5,
    fold_big: 1.6,
    fold_small: 0.9,
    keep_big: 0.65,
    keep_small: 0.6,
    al0: 22.0,
    al1: 40.0,
    crease_w: 0.9,
    rib_dim: 0.35,
    clus_f: 9.0,
    cell_big: 0.032,
    len0: 0.04,
    len1: 0.2,
    wid0: 0.012,
    wid1: 0.024,
    off_p: 0.7,
    taper: 0.35,
    reach: 1.0,
    weak_side: 0.15,
    edge_px: 0.0011,
    soft: 0.35,
    fade_u: 0.5,
    body: 0.25,
    g_core: 0.9,
    k_core: 0.35,
    k_cliff: 0.6,
    disp: 0.06,
    k_disp: 0.4,
    g_out: 1.0,
    glint_far: 1.1,
    far_w: 0.1,
    k_out: 0.6,
    tilt_f: 0.2,
    floor: 0.98,
    out_rise: 0.3,
    outer_mid: 0.35,
    tilt: 0.2,
    panel_var: 0.25,
    crease: 0.2,
    trough: 0.35,
    trough_w: 0.03,
    sheen: 0.6,
    sheen_d: 0.22,
    sheen_w: 0.07,
    broad: 0.25,
    rib_in: 0.9,
    rib_out: 0.6,
    rib_w: 0.012,
    rib_lift: 0.3,
    knot: 0.75,
    seam: 0.9,
    wrinkle: 0.05,
    wr_pitch: 0.03,
    rumple: 0.45,
    rim_dim: 0.97,
    hem: 0.5,
    bound: 0.3,
    weave_in: 0.1,
    weave_out: 0.14,
    weave_pitch: 0.0075,
    crinkle_in: 0.1,
    crinkle_out: 0.08,
    pleat: 0.04,
    cglint: 0.25,
    cg_t: 0.84,
    sparkle: 2.0,
    spark_p: 0.0002,
    scratch: 0.3,
    scr_t: 0.9,
    size: 1.0,
    seam_from: 0.7,
    seam_w: 0.0022,
    grain: 1.5,
    pucker: 0.0,
    seam_ridge: 0.0,
    stitch: 0.0,
    seam_light: 0.0,
};

/// A shallow silver bowl's dull fabric (`DeepSilver::shallow`): the deep
/// bowl's gores, ribs, folds and foil, without their rise toward the rim (the
/// shallow bowl's hot ring, `Modifier::silver_profile_at`, sets the level
/// over the radius); its cloth's features at their size on RLS025's 105 cm
/// bowl. As RLS025's: no lighter panel middle (the light runs beside the
/// ribs and seams, `Modifier::silver_umbrella`), the ribs bright lines where
/// the bowl mirrors the flash, the seams sewn ridges from near the hub that
/// pucker where the ribs pull across them (`seam_height`, lit), and
/// the foil grainy, and each gore curved more steeply away from its ribs
/// (a broad dark trough beside each, one side lighter).
const SHALLOW: Look = Look {
    out_rise: 0.0,
    outer_mid: 0.0,
    rib_in: -0.6,
    seam: 1.0,
    seam_w: 0.004,
    pucker: 0.014,
    seam_ridge: 0.0015,
    seam_light: 1.2,
    stitch: 2.5,
    size: 165.0 / 105.0,
    seam_from: 0.06,
    grain: 1.0,
    trough: 0.5,
    trough_w: 0.06,
    tilt: 0.4,
    ..DEEP
};

/// One fold size: its layer number (for the hashes), the cell along a rib
/// that holds at most one fold, the length and width ranges, the share of
/// cells holding one and its strength.
struct FoldSize {
    layer: i32,
    cell: f32,
    lens: (f32, f32),
    wids: (f32, f32),
    keep: f32,
    amp: f32,
}

const FOLD_SIZES: [FoldSize; 2] = [
    FoldSize { layer: 0, cell: DEEP.cell_big, lens: (DEEP.len0, DEEP.len1), wids: (DEEP.wid0, DEEP.wid1), keep: DEEP.keep_big, amp: DEEP.fold_big },
    FoldSize { layer: 1, cell: 0.013, lens: (0.02, 0.05), wids: (0.006, 0.011), keep: DEEP.keep_small, amp: DEEP.fold_small },
];

use crate::patterns::smooth;

use crate::patterns::gauss;

/// value2 at the prototype's spread (`NOISE_MATCH`), in about [0, 1].
fn vn(x: f32, y: f32, seed: u32) -> f32 {
    0.5 + NOISE_MATCH * (value2(x, y, seed) - 0.5)
}

/// fbm2 at the prototype's spread, centred: about -0.5..0.5.
fn fbm(x: f32, y: f32, octaves: u32, seed: u32) -> f32 {
    NOISE_MATCH * (fbm2(x, y, octaves, seed) - 0.5)
}

/// The signed distance to a box of half extents (`hx`, `hy`) centred on
/// (`cx`, `cy`), its corners rounded by `rad`. Negative inside.
fn rbox(x: f32, y: f32, cx: f32, cy: f32, hx: f32, hy: f32, rad: f32) -> f32 {
    let qx = (x - cx).abs() - hx + rad;
    let qy = (y - cy).abs() - hy + rad;
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - rad
}

/// The distance from (`x`, `y`) to the segment from a to b.
fn seg(x: f32, y: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (px, py, vx, vy) = (x - ax, y - ay, bx - ax, by - ay);
    let h = ((px * vx + py * vy) / (vx * vx + vy * vy)).clamp(0.0, 1.0);
    (px - vx * h).hypot(py - vy * h)
}

/// One long tent crease of a panel: where across the panel it runs (and
/// how it wanders along the radius), the radii it spans, its strength and
/// how far across the panel its slope reaches.
#[derive(Clone, Copy, Debug, Default)]
struct Crease {
    centre: f32,
    wander_rate: f32,
    wander_phase: f32,
    from: f32,
    to: f32,
    amp: f32,
    reach: f32,
}

/// A panel's own values: its lobe's and its fabric's level, its two rumple
/// slants (cosine and sine) and its tent creases.
#[derive(Clone, Copy, Debug)]
struct Panel {
    lobe: f32,
    shade: f32,
    rumple: [(f32, f32); 2],
    creases: [Crease; CREASES],
}

/// One crease fold, anchored on a rib at `a0` from the hub: the cosine and
/// sine of its slant back toward the hub, its length and width, its strength
/// (0 for a cell without a fold), its bend, how far off the rib it starts,
/// where it starts to fade toward its far end, and its brightness ripple's
/// phase.
#[derive(Clone, Copy, Debug, Default)]
struct Fold {
    a0: f32,
    ca: f32,
    sa: f32,
    len: f32,
    width: f32,
    amp: f32,
    bend: f32,
    t0: f32,
    end: f32,
    phase: f32,
}

/// One fold size's table: the cells per rib side, each side's slant (as a
/// cotangent, which finds a point's cell) and the folds, side by side.
#[derive(Clone, Debug)]
struct FoldTable {
    /// The cell along a rib, at the look's size.
    cell: f32,
    cells: usize,
    cot: Vec<f32>,
    folds: Vec<Fold>,
}

#[derive(Clone, Debug)]
pub struct DeepSilver {
    /// The ribs, half a panel's angle, its cosine and sine.
    ribs: usize,
    half: f32,
    cos_half: f32,
    sin_half: f32,
    seed: u32,
    panels: Vec<Panel>,
    /// Each rib side's fold strength: one side of a rib carries most of its
    /// folds, so the two sides never mirror into a feather.
    side_amp: Vec<f32>,
    tables: [FoldTable; 2],
    /// The face's mean over the polygon, with the stand's colour in units of
    /// the result (`new`), which `factor` divides by; for the dull fabric
    /// alone (`shallow`), its mean, which `fabric` divides by.
    level: f32,
    look: &'static Look,
}

impl DeepSilver {
    /// The face of an umbrella with `ribs` ribs (at least 3), its folds and
    /// glints from `seed`.
    pub fn new(ribs: u32, seed: u32) -> DeepSilver {
        let mut deep = Self::build(ribs, seed, &DEEP);
        // The level: the face behind the stand and the stand's colour,
        // averaged over the polygon. The stand's colour is in units of the
        // final mean, so the level solves for it.
        let n = TABLE_SAMPLES;
        let (mut face_sum, mut stand_sum, mut count) = (0.0f64, 0.0f64, 0u32);
        for j in 0..n {
            for i in 0..n {
                let x = (i as f32 + 0.5) / n as f32 * 2.0 - 1.0;
                let y = (j as f32 + 0.5) / n as f32 * 2.0 - 1.0;
                if let Some(face) = deep.face(x, y, false) {
                    let (alpha, colour) = deep.stand(x, y);
                    face_sum += (face * (1.0 - alpha)) as f64;
                    stand_sum += (alpha * colour) as f64;
                    count += 1;
                }
            }
        }
        if count > 0 {
            let (face_mean, stand_mean) = (face_sum / count as f64, stand_sum / count as f64);
            deep.level = ((face_mean / (1.0 - stand_mean).max(0.5)) as f32).max(1.0e-6);
        }
        deep
    }

    /// A shallow silver bowl's dull fabric alone (`fabric`): the deep bowl's
    /// curved gores, ribs, rib folds, rumples, wrinkle bands and foil, with
    /// no flash's reflection and no stand, its mean 1 over the polygon.
    pub fn shallow(ribs: u32, seed: u32) -> DeepSilver {
        let mut foil = Self::build(ribs, seed, &SHALLOW);
        let n = TABLE_SAMPLES;
        let (mut sum, mut count) = (0.0f64, 0u32);
        for j in 0..n {
            for i in 0..n {
                let x = (i as f32 + 0.5) / n as f32 * 2.0 - 1.0;
                let y = (j as f32 + 0.5) / n as f32 * 2.0 - 1.0;
                if let Some(face) = foil.face(x, y, true) {
                    sum += face as f64;
                    count += 1;
                }
            }
        }
        foil.level = if count > 0 { ((sum / count as f64) as f32).max(1.0e-6) } else { 1.0 };
        foil
    }

    fn build(ribs: u32, seed: u32, look: &'static Look) -> DeepSilver {
        let ribs = ribs.max(3) as usize;
        let half = std::f32::consts::PI / ribs as f32;
        let panels = (0..ribs as i32)
            .map(|k| {
                let creases = std::array::from_fn(|ci| {
                    let hk = |f: i32| hash01(k, ci as i32, f, seed.wrapping_add(13));
                    let from = 0.1 + 0.5 * hk(4);
                    Crease {
                        centre: -0.75 + 1.5 * hk(1),
                        wander_rate: 6.0 + 8.0 * hk(2),
                        wander_phase: 6.28 * hk(3),
                        from,
                        to: from + 0.25 + 0.6 * hk(5),
                        amp: 0.4 + 0.6 * hk(6),
                        reach: 0.15 + 0.35 * hk(7),
                    }
                });
                let rumple = std::array::from_fn(|fam| {
                    let sign = if fam == 1 { 1.0 } else { -1.0 };
                    let an = (4.0 + 12.0 * hash01(k, fam as i32, 17, seed)) * DEG * sign;
                    (an.cos(), an.sin())
                });
                Panel {
                    lobe: 1.0 + look.lobe_var * (2.0 * hash01(k, 79, 0, seed) - 1.0),
                    shade: 1.0 + look.panel_var * (2.0 * hash01(k, 77, 0, seed) - 1.0),
                    rumple,
                    creases,
                }
            })
            .collect();
        let sides = 2 * ribs;
        let side_amp = (0..sides)
            .map(|q| {
                // The rib a side belongs to: a panel's + side is the rib after
                // it, its - side the rib before.
                let rib = if q % 2 == 1 { q } else { (q + sides - 1) % sides };
                let dominant = (hash01(rib as i32, 5, 0, seed) < 0.5) == (q % 2 == 1);
                if dominant {
                    0.3 + 0.7 * hash01(q as i32, 99, 0, seed)
                } else {
                    look.weak_side * (0.5 + hash01(q as i32, 98, 0, seed))
                }
            })
            .collect();
        let tables = [Self::fold_table(&FOLD_SIZES[0], sides, half, seed, look), Self::fold_table(&FOLD_SIZES[1], sides, half, seed, look)];
        DeepSilver { ribs, half, cos_half: half.cos(), sin_half: half.sin(), seed, panels, side_amp, tables, level: 1.0, look }
    }

    /// The folds of one size on every rib side, a cell at a time along the
    /// rib (four cells before the hub, so the slanted ones reaching back are
    /// found). A rib side's folds share a slant (with a little jitter per
    /// fold) and gather in runs (a smooth noise along the rib); each side's
    /// fold zone sits a little in or out.
    fn fold_table(size: &FoldSize, sides: usize, half: f32, seed: u32, look: &Look) -> FoldTable {
        let k = look.size;
        let (cell, lens, wids) = (size.cell * k, (size.lens.0 * k, size.lens.1 * k), (size.wids.0 * k, size.wids.1 * k));
        let cells = (1.1 / cell) as usize + 8;
        let lay = size.layer;
        let sin_half = half.sin();
        let mut cot = Vec::with_capacity(sides);
        let mut folds = Vec::with_capacity(sides * cells);
        for q in 0..sides as i32 {
            let al_side = (look.al0 + (look.al1 - look.al0) * hash01(q, lay, 3, seed)) * DEG;
            cot.push(al_side.cos() / al_side.sin());
            let zone = (hash01(q, lay, 21, seed) - 0.5) * 0.14;
            for j in 0..cells as i32 {
                let cell_j = j - 4;
                let hk = |f: i32| hash01(q, cell_j, 100 + lay * 32 + f, seed);
                let a0 = (cell_j as f32 + 0.15 + 0.7 * hk(2)) * cell;
                let clus = smooth(0.3, 0.7, vn(a0 * look.clus_f + 0.37, q as f32 * 7.31 + lay as f32 * 3.3 + 0.5, seed.wrapping_add(3)));
                let exists = hk(1) < size.keep * (0.1 + 0.9 * clus);
                let al = al_side + 20.0 * DEG * (hk(3) - 0.5);
                let (sa, ca) = (al.sin(), al.cos());
                // It ends near the panel's centreline.
                let longest = (look.reach * a0 * sin_half / (sa + ca * sin_half)).max(0.001);
                let len = (lens.0 * (lens.1 / lens.0).powf(hk(4))).max(0.001).min(longest);
                let width = (wids.0 + (wids.1 - wids.0) * hk(5)) * (len / lens.1).powf(0.3);
                let aw = a0 - zone;
                let window = smooth(0.1, 0.2, a0) * (0.15 + 0.85 * smooth(0.24, 0.38, aw)) * (1.0 - 0.55 * smooth(0.55, 0.75, aw)) * (1.0 - smooth(0.86, 0.94, a0));
                let amp = if exists && len > 0.008 { size.amp * (0.35 + 0.65 * hk(6).powf(0.7)) * window * (0.7 + 0.3 * clus) } else { 0.0 };
                let t0 = (if hk(8) < look.off_p { hk(9) * 0.7 } else { 0.0 }) * len;
                folds.push(Fold { a0, ca, sa, len, width, amp, bend: (hk(7) - 0.5) * 0.3, t0, end: 0.45 + 0.35 * hk(10), phase: hk(11) * 6.28 });
            }
        }
        FoldTable { cell, cells, cot, folds }
    }

    /// The folds of one size at a point `a` along its nearest rib from the
    /// hub and `d` into the panel, on rib side `q`, `hw` from the rib to the
    /// panel's centreline: the bright bars (G) and the dark creases (K).
    /// A fold is a crisp bar: hard edges about a pixel wide, brightest at its
    /// lip (the rim side), pointed at its far end, a softly lit broad flank
    /// on its hub side, a hard dark crease beyond the lip.
    fn folds(&self, table: &FoldTable, a: f32, d: f32, q: usize, hw: f32) -> (f32, f32) {
        let look = self.look;
        let (e, cw, cell) = (look.edge_px * look.size, look.crease_w, table.cell);
        let jq = ((a + d * table.cot[q]) / cell).floor() as i32 + 4;
        let (mut g, mut k) = (0.0f32, 0.0f32);
        for dj in -2..=2 {
            let j = (jq + dj).clamp(0, table.cells as i32 - 1) as usize;
            let f = &table.folds[q * table.cells + j];
            if f.amp <= 0.0 {
                continue;
            }
            let t = -(a - f.a0) * f.ca + d * f.sa - f.t0;
            let u = t / f.len;
            let n = (a - f.a0) * f.sa + d * f.ca - f.bend * f.len * u * u;
            if !(u > -0.05 && u < 1.02 && n > -3.2 * f.width - 2.0 * e && n < (cw + 0.3) * f.width + 2.0 * e) {
                continue;
            }
            let uc = u.clamp(0.0, 1.0);
            let wl = f.width * (1.0 - look.taper * uc * uc) + e;
            let below_lip = 1.0 - smooth(-e, e, n);
            let bar = smooth(-wl, -wl + look.soft * wl + e, n) * below_lip * (0.55 + 0.45 * ((n + wl) / wl).clamp(0.0, 1.0));
            let bar = bar.max(look.body * gauss(n + 0.6 * wl, 1.3 * wl) * below_lip);
            let dk = cw * wl;
            let dark = smooth(-e, e, n) * (1.0 - smooth(dk - 1.5 * e, dk + 1.5 * e, n)) * (1.0 - 0.4 * (n / dk).clamp(0.0, 1.0));
            let env = smooth(-0.04, 0.04, u) * (1.0 - smooth(f.end, 1.0, u));
            let lit = (look.rib_dim + (1.0 - look.rib_dim) * smooth(-0.02, 0.2, u)) * (1.0 - look.fade_u * uc);
            let ripple = 0.8 + 0.2 * (u * 6.0 + f.phase).sin();
            let amp = f.amp * env;
            g = g.max(amp * lit * ripple * bar);
            k = k.max(amp.min(1.0) * dark);
        }
        let fade = smooth(0.0, 0.1, (hw - d) / hw.max(1.0e-4));
        (g * fade, k * fade)
    }

    /// The seam down a panel's middle as the cloth's height at `along` the
    /// seam from the hub and `across` it (box units), on panel `kf`, from the
    /// panel's tension map (the owner's fabric recipe): the two rib tips at
    /// the panel's corners pull the cloth toward them (weighted 1 / r^2), the
    /// hub holds it along the seam. Where the pull runs along the seam (near
    /// the hub) it is a taut, straight sewn ridge; where the tips pull across
    /// it (from about a third of the radius out) the cloth is slack along it
    /// and the seam puckers: short folds whose crests run along the tension,
    /// across the seam, sampled in that frame (stretched along the tension,
    /// squeezed across it, domain-warped, a crease profile), a little longer
    /// the slacker the seam, in runs; the hem holds the seam's end flat.
    /// Lengths at the look's `size`.
    fn seam_height(&self, along: f32, across: f32, kf: f32) -> f32 {
        let look = self.look;
        let sz = look.size;
        let seed = |k: u32| self.seed.wrapping_add(k);
        // The tension map: the tips' pull along and across the seam, and the hub's hold.
        let (mut t_along, mut t_across) = (1.0f32, 0.0f32);
        for side in [-1.0f32, 1.0] {
            let (va, vc) = (self.cos_half - along, side * self.sin_half - across);
            let d2 = va * va + vc * vc + 0.01;
            let d = d2.sqrt();
            let w = 1.0 / d2;
            t_along += w * va.abs() / d;
            t_across += w * vc.abs() / d;
        }
        let slack = t_across / (t_along + t_across);
        let on = smooth(look.seam_from - 0.02, look.seam_from + 0.02, along) * (1.0 - smooth(0.88, 0.96, along));
        // The sewn ridge, wandering a little where the seam is slack.
        let wander = 0.003 * sz * slack * fbm(along * 15.0 / sz + kf * 2.7, kf * 1.3, 2, seed(141));
        let ridge = look.seam_ridge * sz * gauss(across - wander, 0.003 * sz);
        // The puckers in the tension frame: along the tension is across the
        // seam (stretched: the folds run long that way), across the tension
        // is along the seam (squeezed: a fold every `spacing`).
        let spacing = 0.03 * sz;
        let reach = (0.015 + 0.025 * slack) * sz;
        let warp = fbm(along * 6.0 / sz + kf * 3.3, across * 20.0 / sz, 2, seed(151));
        let ut = across / reach * 0.35 + 2.0 * warp;
        let vt = along / spacing + 1.5 * warp + kf * 7.1;
        let v = (fbm2(ut, vt, 3, seed(161)) - 0.5) * 1.6 + 0.5;
        let v = v.clamp(0.0, 1.0);
        let profile = 0.7 * (1.0 - (2.0 * v - 1.0).abs()) + 0.3 * v;
        let runs = smooth(0.25, 0.5, vn(along * 4.0 / sz + kf * 3.1, kf * 5.7, seed(171)));
        let puckers = look.pucker * sz * smooth(0.25, 0.45, slack) * runs * gauss(across, reach) * profile;
        on * (ridge + puckers)
    }

    /// The face at (`x`, `y`) before the level and without the stand; None
    /// outside the polygon. With `fabric` only the dull fabric: no flash's
    /// reflection, its glints or its brightening of the ribs and foil.
    fn face(&self, x: f32, y: f32, fabric: bool) -> Option<f32> {
        let look = self.look;
        let sz = look.size;
        let half = self.half;
        let r = x.hypot(y);
        let theta = y.atan2(x);
        let turn = (theta / (2.0 * half)).round();
        let c = turn * 2.0 * half;
        let dphi = theta - c;
        let (cos_d, sin_d) = (dphi.cos(), dphi.sin());
        if !(r * cos_d <= self.cos_half) {
            return None;
        }
        let seed = |k: u32| self.seed.wrapping_add(k);
        // The panel: across it (`s`, -1..1 rib to rib), its number, which way
        // its middle faces (`b`, 1 for the panel straight left), and the
        // angle in panel units from a rib (`ua`).
        let s = dphi / half;
        let k = (turn as i32).rem_euclid(self.ribs as i32) as usize;
        let kf = k as f32;
        let panel = &self.panels[k];
        let b = -c.cos();
        let ua = (theta - half).rem_euclid(std::f32::consts::TAU) / (2.0 * half);
        let (along, across) = (r * cos_d, r * sin_d);
        // The nearest rib: along it from the hub (`a`), into the panel (`d`),
        // the panel's half width there (`hwf`) and the rib side `q`.
        let a = r * (half - dphi.abs()).cos();
        let d = r * (half - dphi.abs()).sin();
        let hwf = r * self.sin_half;
        let q = k * 2 + usize::from(s > 0.0);
        let sgn = if s > 0.0 { 1.0 } else { -1.0 };

        // ------------------------------------------------ the slow fields
        let jit = fbm(s * 2.2 + kf * 7.7, kf * 3.1 + 0.5, 3, seed(41));
        let calm = 0.25 + 0.75 * smooth(0.15, 0.4, r);
        let st1 = fbm(ua * 1.6, r * 1.4, 3, seed(37));
        let st2 = fbm(ua * look.fine_n, r * 3.0, 2, seed(53));
        let fstr = fbm(ua * look.fine_n * 1.8, r * 2.5, 2, seed(57));
        let streak = (calm * (look.streak * 2.0 * st1 + look.streak_fine * 2.0 * st2)).exp();
        let cr = fbm(s * 3.0 + kf * 5.1, r * 0.7 + kf * 0.37, 3, seed(23));
        let cr2 = fbm(s * 5.0 + kf * 1.9, r * 1.2, 2, seed(71));
        let ocrease = (look.crease * (8.0 * cr).tanh() + 0.3 * look.crease * (8.0 * cr2).tanh()).exp();
        let warp = fbm(s * 1.5 + kf * 3.3, r * 3.0, 3, seed(29));
        let clus = smooth(0.4, 0.7, 0.5 + fbm(s * 1.8 + kf * 2.1, r * 3.5, 3, seed(31)));
        let crinkle = fbm(along * (70.0 / sz) + kf * 3.1, across * (70.0 / sz), 2, seed(61));
        let broad = fbm(along * 3.5 + kf * 1.3, across * 5.0 + kf * 0.7, 3, seed(91));
        // Rumples: soft elongated creases near the radius in two slants, each
        // with a crisp lit crest and a shadowed side (a ridged noise embossed
        // across its run).
        let mut rumple = 0.0f32;
        for (fam, &(ca, sa)) in panel.rumple.iter().enumerate() {
            let famf = fam as f32;
            let u_ = along * ca + across * sa;
            let v_ = -along * sa + across * ca;
            let ph = v_ * (32.0 / sz) + 0.5 + fbm(u_ * 2.5 + kf * 2.3 + famf * 5.0, v_ * 4.0, 2, seed(111 + fam as u32));
            let ridge = |dv: f32| (1.0 - (std::f32::consts::PI * (ph + dv * (32.0 / sz))).sin().abs()).powi(5);
            let run = smooth(0.55, 0.75, 0.5 + fbm(u_ * (10.0 / sz) + famf * 3.1, v_ * (20.0 / sz) + kf * 1.1, 2, seed(121 + fam as u32)));
            rumple += (ridge(0.0) - ridge(0.005 * sz)) * run * (0.5 + 0.5 * famf);
        }
        let mesh_amp = 0.6 + 0.8 * (0.5 + fbm(along * 12.0 + kf * 1.7, across * 12.0, 2, seed(67)));
        let mesh_warp = fbm(along * 20.0, across * 20.0 + kf * 2.9, 2, seed(73));
        // The long tent creases nearly along the radius: the slope across the
        // panel of a few near-radial folds.
        let mut ga = 0.0f32;
        for cr in &panel.creases {
            let centre = cr.centre + 0.06 * (r * cr.wander_rate + cr.wander_phase).sin();
            let amp = cr.amp * smooth(cr.from - 0.05, cr.from + 0.05, r) * (1.0 - smooth(cr.to - 0.08, cr.to + 0.08, r));
            let ds = s - centre;
            let sign = if ds > 0.0 {
                1.0
            } else if ds < 0.0 {
                -1.0
            } else {
                0.0
            };
            ga -= amp * sign * (-ds.abs() / cr.reach).exp() * (1.0 - gauss(ds, 0.03));
        }
        let ga = look.crease_a * ga;

        // ------------------------------------------------ the crease folds on the ribs
        let (mut g, mut kd) = (0.0f32, 0.0f32);
        if r > 0.07 && r < 0.95 {
            for table in &self.tables {
                let (gl, kl) = self.folds(table, a, d, q, hwf);
                g = g.max(gl * self.side_amp[q]);
                kd = kd.max(kl * self.side_amp[q]);
            }
        }
        let g = g.min(1.5);
        let kc = kd.min(1.0);

        // ------------------------------------------------ the flash's reflection
        // Across the panel: near the hub the core fills the panel; further
        // out it gathers toward the panel's side nearer the image bottom, and
        // the lobe ends a little sooner beside the ribs.
        let open = smooth(0.22, 0.45, r);
        let s_eff = s + 0.25 * ga;
        let sw = look.sw0 + (look.sw_hub - look.sw0) * (1.0 - open);
        let cshape = (-((s_eff - look.core_shift * open * b) / sw).abs().powi(6)).exp();
        let edge = look.e0 + look.e_up * c.sin() + look.e_slant * b * s - look.e_rib * (1.0 - cshape) + look.e_jit * jit + look.e_fstr * fstr;
        let wedge = smooth(0.2, 0.32, r) * (1.0 - smooth(0.62, 0.75, r));
        let r_eff = r - look.disp * (g - look.k_disp * kc) * wedge;
        let sig = 1.0 / (1.0 + (-(edge - r_eff) / look.edge_w).exp());
        let tail = look.tail_a * (-(r_eff - edge).max(0.0) / look.tail_w).exp();
        let env = (sig + (1.0 - sig) * tail) * (1.0 - look.hub_fall * smooth(0.2, 0.42, r));
        let cross = look.cmin + (1.0 - look.cmin) * cshape;
        let stk = sig * streak + (1.0 - sig) * streak.powf(0.25);
        // The strobe's own body shades the flash from the gores straight below it.
        let hd = (theta + std::f32::consts::FRAC_PI_2).abs();
        let hshade = 1.0 - look.hshade * (-(hd / 0.38).powi(4)).exp() * smooth(0.05, 0.15, r) * (1.0 - smooth(0.35, 0.5, r));
        let mut spec = look.plateau * panel.lobe * env * cross * stk * hshade;
        // The folds: crests a little brighter; creases dark, harder at the
        // cliff than in the clipped core.
        let core = smooth(0.6, 0.95, sig);
        let kk = (look.k_core * core + look.k_cliff * (1.0 - core)) * smooth(0.14, 0.3, r);
        spec *= (1.0 + look.g_core * g) * (1.0 - kk * kc);
        let tilt = look.tilt_k * ga.abs() / 0.25;
        spec = spec * (look.facet_spec * ga).exp() / (1.0 + tilt * tilt * open + (look.tilt_f * kd).powi(2));

        // ------------------------------------------------ the dull fabric: a curved gore
        // The side of the rib that faces up catches the room's light: a sheen
        // band beside it, and that half of the gore is lighter than the other.
        let rib_angle = c + sgn * half;
        let upside = if theta.sin() < rib_angle.sin() { 1.0 } else { 0.0 };
        let dn = d / hwf.max(1.0e-3);
        let sheen = look.sheen * upside * gauss(dn - look.sheen_d, look.sheen_w) * smooth(0.3, 0.5, r);
        let trough = 1.0 - look.trough * (-d / (look.trough_w * sz * (0.3 + r))).exp();
        let gore = (1.0 + look.outer_mid * smooth(0.9, 0.2, s.abs()) + look.tilt * (upside - 0.5) * (1.0 - dn)) * trough + sheen;
        let mut outer = look.floor * gore * (1.0 + look.out_rise * smooth(0.55, 0.85, r)) * (1.0 + look.tilt * 0.5 * b * s) * panel.shade * ocrease * (1.0 + look.broad * 2.0 * broad);
        outer *= 1.0 + look.facet_diff * ga / 0.25;
        // The seam's height lit by the flash below the hub: a slope facing it
        // brighter, one facing away darker (the normal from the height's slope).
        if look.pucker > 0.0 || look.seam_ridge > 0.0 {
            let e = 0.0015 * sz;
            let h0 = self.seam_height(along, across, kf);
            let ha = (self.seam_height(along + e, across, kf) - h0) / e;
            let hc = (self.seam_height(along, across + e, kf) - h0) / e;
            // The slope in the face (the seam's along and across directions at the panel's angle `c`).
            let (sc, cc) = c.sin_cos();
            let (gx, gy) = (ha * cc - hc * sc, ha * sc + hc * cc);
            let (lx, ly) = (-x, -0.1 - y);
            let ll = lx.hypot(ly).max(1.0e-3);
            outer *= (-look.seam_light * (gx * lx + gy * ly) / ll).clamp(-1.0, 1.0).exp();
        }
        // The stitched fold: a groove of depth `w` and width `w` down the
        // seam, its walls' slope (the gaussian's, peak about 0.86 at
        // 0.7 w off the seam) darkening the mirror foil; deepest near the hub,
        // where the seam is taut, shallower out where it puckers.
        if look.stitch > 0.0 {
            let w = 0.008 * sz;
            let xs = across / w;
            let slope = 2.0 * xs.abs() * (-xs * xs).exp();
            let on = smooth(look.seam_from - 0.02, look.seam_from + 0.02, along) * (1.0 - smooth(0.88, 0.96, along));
            let taut = 1.0 - 0.5 * smooth(0.3, 0.7, r);
            outer *= (-look.stitch * slope * on * taut).exp();
        }
        outer *= (1.0 + look.g_out * g - look.k_out * kc).clamp(0.3, 3.0);
        // Beyond the lobe a fold's crest can still face the flash: isolated
        // glint bars on the dull fabric, fading outward.
        let far = (-(r - edge).max(0.0) / look.far_w).exp() * (1.0 - sig) * smooth(0.15, 0.3, r);
        let glint = look.plateau * look.glint_far * panel.lobe * far * g.min(1.2) * (1.0 - look.k_cliff * kc);
        // Wavy wrinkle bands across the gore, in clusters: a soft face toward
        // the light and a crisper shadowed face.
        let wph = (along + 0.35 * across * sgn + 0.06 * warp * 4.0) / (look.wr_pitch * sz) * (1.0 + 0.25 * warp);
        let fr = wph - wph.floor();
        let wshape = (if fr < 0.7 { smooth(0.0, 0.7, fr) } else { 1.0 - smooth(0.7, 0.82, fr) }) * 2.0 - 1.0;
        let wamp = look.wrinkle * clus * smooth(0.72, 0.85, r) * (1.0 - smooth(0.95, 0.985, r));
        outer *= (1.0 + wamp * wshape) * (1.0 + look.rumple * rumple * smooth(0.2, 0.4, r));
        let (spec, glint, env) = if fabric { (0.0, 0.0, 1.0) } else { (spec, glint, env) };
        let mut l = outer + spec + glint;
        // The fabric alone (a shallow bowl): bright where its ring mirrors the
        // flash, out to about two thirds of the radius.
        let bright = if fabric { 1.0 - smooth(0.35, 0.7, r) } else { smooth(0.0, 0.5, env) };

        // ------------------------------------------------ ribs: thin dark lines (thinner in the core), a lit edge
        let w = look.rib_w * sz * (1.0 - 0.3 * smooth(0.45, 0.75, r)) * (0.75 + 0.25 * smooth(0.1, 0.45, r)) * (0.45 + 0.55 * smooth(0.02, 0.14, r));
        let rib_dark = look.rib_out + (look.rib_in - look.rib_out) * bright;
        l *= 1.0 - rib_dark.clamp(0.0, 0.97) * gauss(d, 0.4 * w);
        l *= 1.0 + look.rib_lift * upside * gauss(d - 0.8 * w, 0.3 * w) * (1.0 - 0.7 * bright);
        let knot = gauss(a - 0.45, 0.007 * sz) * gauss(d, 0.7 * w);
        l *= 1.0 - (1.0 - look.knot) * knot;

        // ------------------------------------------------ the outer zone: seam, rim, the thin light bound edge
        let seam = 1.0 - (1.0 - look.seam) * gauss(r * sin_d.abs(), look.seam_w * sz) * smooth(look.seam_from - 0.02, look.seam_from + 0.02, r);
        let rim = 1.0 - (1.0 - look.rim_dim) * smooth(0.86, 0.96, r);
        let hem_d = self.cos_half - along;
        let bound = 1.0 + look.bound * gauss(hem_d - 0.011, 0.0035);
        l *= seam * rim * bound;

        // ------------------------------------------------ fine fabric: woven foil mesh, crinkle, crinkle glints, sparkle
        let pw = look.weave_pitch * sz;
        let mw = mesh_warp * 0.6;
        let pi = std::f32::consts::PI;
        let mesh = 0.5 * ((pi * (along / pw + mw)).cos().abs().powi(8) + (pi * (across / pw - mw)).cos().abs().powi(8)) - 0.12;
        l *= 1.0 - (look.weave_out + (look.weave_in - look.weave_out) * bright) * mesh_amp * mesh;
        l *= 1.0 + (look.crinkle_out + (look.crinkle_in - look.crinkle_out) * bright) * 2.0 * crinkle;
        l *= 1.0 + look.pleat * (pi * 6.0 * (s + 1.0)).sin() * (1.0 - smooth(0.12, 0.2, r));
        // Crinkle glints: small crisp dashes across the radius where a crinkle
        // faces the flash; more near the light.
        let (ca, sa) = ((25.0 * DEG).cos(), (25.0 * DEG).sin() * sgn);
        let cgn = vn((along * ca + across * sa) * (160.0 / sz) + kf * 3.3, (across * ca - along * sa) * (45.0 / sz) + kf * 7.1, seed(101));
        let cmask = smooth(look.cg_t, look.cg_t + 0.03, cgn) * clus;
        let near_light = smooth(0.3, 0.45, r) * (1.0 - 0.6 * smooth(0.55, 0.9, r)) * (1.0 - smooth(0.97, 0.99, r));
        l *= 1.0 + look.cglint * cmask * near_light * (1.0 - bright);
        // Sparkle: a few pinpoint glints in the foil, in cells of a 700 pixel
        // face.
        let (ix, iy) = (((x + 1.0) * (350.0 / sz)).floor() as i32, ((y + 1.0) * (350.0 / sz)).floor() as i32);
        if bright < 0.5 && hash01(ix, iy, 0, seed(7)) < look.spark_p * smooth(0.3, 0.6, r) {
            l *= 1.0 + look.sparkle;
        }
        // Sparse fine scratches in the foil, near the radius in two slants:
        // bright on the grey, dark on the white.
        for (fam, an) in [(0u32, 9.0 * DEG), (1, -14.0 * DEG)] {
            let famf = fam as f32;
            let (cn, sn) = (an.cos(), an.sin());
            let u_ = along * cn + across * sn;
            let v_ = -along * sn + across * cn;
            let sc = vn(u_ * (9.0 / sz) + kf * 3.7 + famf * 11.0, v_ * (340.0 / sz) + kf * 5.3, seed(81 + fam));
            let line = smooth(look.scr_t, look.scr_t + 0.06, sc);
            l *= 1.0 + look.scratch * line * (1.0 - 1.7 * bright) * (0.3 + 0.7 * smooth(0.0, 0.3, env));
        }
        // The grain: a facet per grain, each at its own tilt (mean 1).
        if look.grain > 0.0 {
            let (gx, gy) = (((x + 1.0) * (156.0 / sz)).floor() as i32, ((y + 1.0) * (156.0 / sz)).floor() as i32);
            l *= log_uniform(hash01(gx, gy, 3, seed(131)), look.grain);
        }
        // ------------------------------------------------ the hem
        if hem_d.abs() < 0.004 {
            l *= look.hem;
        }
        Some(l)
    }

    /// The strobe head and the stand in front of the face at (`x`, `y`): how
    /// much they cover (0 or 1) and their linear colour, in units of the
    /// face's mean. Later parts sit in front of earlier ones.
    fn stand(&self, x: f32, y: f32) -> (f32, f32) {
        if x.abs() >= 0.12 {
            return (0.0, 0.0);
        }
        let (mut alpha, mut colour) = (0.0f32, 0.0f32);
        let mut put = |covered: bool, value: f32| {
            if covered {
                alpha = 1.0;
                colour = value;
            }
        };
        // The pole, slightly wider toward the floor, a lighter line down its right side.
        let px0 = -0.002;
        let hwp = 0.017 + 0.006 * ((-y - 0.3) / 0.7).clamp(0.0, 1.0);
        put(y < -0.29 && (x - px0).abs() < hwp, 0.004 + 0.02 * gauss(x - px0 - 0.55 * hwp, 0.004));
        // The clamp at the pole's top, its lever up to the left, and the
        // locking knob lower down.
        put(rbox(x, y, px0, -0.305, 0.021, 0.015, 0.004) < 0.0, 0.006);
        put(seg(x, y, -0.012, -0.30, -0.055, -0.28) < 0.0065, 0.008);
        put(seg(x, y, px0, -0.42, -0.05, -0.415) < 0.006, 0.006);
        let kd = ((x + 0.066) / 0.019).hypot((y + 0.418) / 0.028);
        put(kd < 1.0, 0.006 + 0.03 * (0.8 - kd).clamp(0.0, 1.0) * if y > -0.418 { 1.0 } else { 0.0 });
        // The spigot and its receptacle under the yoke.
        put(rbox(x, y, px0, -0.262, 0.0085, 0.032, 0.002) < 0.0, 0.005);
        put(rbox(x, y, px0, -0.232, 0.021, 0.011, 0.004) < 0.0, 0.007);
        // The yoke: a U under the head, its tilt knob on the right.
        let yo = rbox(x, y, 0.0, -0.155, 0.083, 0.062, 0.035);
        let yi = rbox(x, y, 0.0, -0.138, 0.065, 0.062, 0.025);
        put(yo < 0.0 && yi > 0.0, 0.006 + 0.02 * gauss(yo + 0.0035, 0.0025) * if y > -0.2 { 1.0 } else { 0.0 });
        put(rbox(x, y, 0.088, -0.142, 0.009, 0.032, 0.007) < 0.0, 0.008);
        // The head: a round body seen nearly face on, a lit rim at the top, a
        // front ring, a display and a button.
        let (hx, hy) = (x / 0.079, (y + 0.072) / 0.086);
        let hr = hx.hypot(hy);
        let head = hr < 1.0;
        let top = hy.clamp(0.0, 1.0).powi(2);
        let ring = gauss(hr - 0.93, 0.025);
        let rim = gauss(hr - 0.985, 0.02) * top;
        let head_colour = if (x - 0.042).hypot(y + 0.103) < 0.0075 {
            0.12
        } else if rbox(x, y, 0.0, -0.038, 0.026, 0.012, 0.004) < 0.0 {
            0.045
        } else {
            0.03 + 0.012 * (1.0 - hr) + 0.05 * ring + 0.25 * rim
        };
        put(head, head_colour);
        // The hub cap above the head: the ribs' dark ends.
        let hub_r = 0.02 + 0.004 * (self.ribs as f32 * (y - 0.006).atan2(x) + 0.5).cos();
        put(x.hypot(y - 0.006) < hub_r && !head, 0.008);
        (alpha, colour)
    }

    /// A shallow bowl's dull fabric at `u` (box units, the ribs' ends at
    /// radius 1): mean 1 over the polygon, 1 outside it.
    pub fn fabric(&self, u: Vec2f) -> f32 {
        self.face(u.x, u.y, true).map_or(1.0, |f| f / self.level)
    }

    /// `factor` without the strobe head and its stand in front, which a
    /// light's 3D model draws as geometry (light_model.rs).
    pub fn factor_bare(&self, u: Vec2f) -> f32 {
        self.face(u.x, u.y, false).map_or(0.0, |face| face / self.level)
    }

    /// The face's emission at `u` (box units, the ribs' ends at radius 1) as
    /// a share of the light's: mean 1 over the polygon, 0 outside it, about
    /// 220 in the clipped core, 0.004 to 0.12 on the stand and the head.
    pub fn factor(&self, u: Vec2f) -> f32 {
        match self.face(u.x, u.y, false) {
            Some(face) => {
                let (alpha, colour) = self.stand(u.x, u.y);
                face * (1.0 - alpha) / self.level + alpha * colour
            }
            None => 0.0,
        }
    }
}
