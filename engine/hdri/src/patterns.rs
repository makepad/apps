//! Procedural patterns shared across the generator, in metres on a surface's own plane, seen at a footprint `foot`
//! (m: what one texel of the map covers there): gradient noise and its band-limited FBM, the fades that keep a
//! feature at its mean once the footprint passes it, and what water leaves on a surface: runs, curtains of streaks
//! hanging from a line source (a wall's top, a deck's edge), drips and runs from holes. The room's
//! materials (room_material.rs) and the studio lights' faces (modifier.rs) draw from them.
//!
//! What is here, so a new surface takes it instead of writing another (noise.rs has the noise base):
//! - Shapes and filters: `smooth` (the one smoothstep; makepad's `smoothstep` is the same but for NaN), `gauss`,
//!   `band_covers` (a band box-filtered over a footprint), `mix3`, `mul3`.
//! - Band limits: `lod` (a feature fades to its mean from a quarter to half its wavelength a footprint), `edge` (a line
//!   box-filtered, then its mean once its spacing is unresolved), `over` (the share of a footprint where a band-limited
//!   field passes a threshold, softened by the variance the footprint lost), `hang` (how much of a run still marks a
//!   surface a share of the way down its length).
//! - Noise by footprint: `gn` (2D gradient noise, 16 gradients), `gl` (gn of a wavelength, faded), `fbm` (band-limited
//!   gradient fbm, not renormalised), `h01` (a uniform 0..1 from a hash and a key).
//! - Water: `runs` (wandering runs down a surface), `drips` and `drips_end` (drops hanging under a run), `curtain`
//!   (streaks hanging from a line source, with drips), `from_holes` (runs from a grid of holes).
//! - Wear (masks; the caller picks the colours): `grunge` (patches of more and less dirt), `seam_grime` (grime and dust
//!   along a floor's seam), `skirting_band` and `kick_marks` (a wall's foot), `water_stain` (a water mark and its tide
//!   line), `leak_columns` (leaks from a seam above), `wet_stain` (grime turning to moss, with its colours).
//! - Cloth and foil: `fan_folds` (a pull point's tension folds), `patches` (where the crinkle shows), `broad_folds`
//!   (warped broad soft folds).
//!
//! Kept in their files on purpose, other formulas (merging them would change the pictures):
//! - room_material.rs `voronoi` and `voronoi_near` (sites jittered by 16-bit halves within 0.1..0.9 of a cell: terrazzo,
//!   brick); plaster's inline stipple and peel; epoxy's scratch strokes (the only band-limited scratches).
//! - modifier.rs `gauss` (x x / (sigma sigma): other rounding), `cells`, `cloth_wrinkles` (corner folds of
//!   1.6487 x e^(-x^2/2)), `umbrella_height` (two-octave fans weighted by 1/(rho^2 + 0.02)), `foil_slope` and
//!   `sock_height` (crease slopes, crumple ridges), the fluorescent cover's dust (`fluo_dirt_depth`: a patchy
//!   `fbm` film, thicker at the frame, and its `specks`, one ragged round speck to a cell faded to its mean share past
//!   the footprint, as `from_holes` fades its runs).
//! - open_box.rs `creases` and `facets`, reflector.rs's hinge creases and `tension` (two pulls, cosine folds),
//!   silver_deep.rs `seam_height`, its scratches and fold table: each multiplies its factors in its own order.
//! - floor.rs (the floor's far model): `cell_noise`, `fine_texture`, `speckle`, `glitter`, `ring` and `fade_start`
//!   (makepad-floor's shader functions, which they must equal), the overgrowth tables.
//! - imperfections.rs (a light's face): `wrinkles`, `f_hash`/`f_noise`.
use crate::noise::{fbm2, hash3_u32};
use makepad_draw::{vec3f, Vec3f};

/// Smoothstep: 0 below `a`, 1 above `b`, the cubic 3t^2 - 2t^3 between (`.clamp`: a NaN stays NaN; makepad's
/// `smoothstep`, `.max(0).min(1)`, gives 0 for it and is otherwise the same bits).
#[inline]
pub(crate) fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// exp(-(x / sigma)^2): 1 at 0, 1/e at sigma.
#[inline]
pub(crate) fn gauss(x: f32, sigma: f32) -> f32 {
    let q = x / sigma;
    (-q * q).exp()
}

/// The share of the box `x - w ..= x + w` that `lo ..= hi` covers, 0..=1: a band (a rod, a strip, a line) box-filtered
/// over a footprint `2 w` wide.
#[inline]
pub(crate) fn band_covers(x: f32, lo: f32, hi: f32, w: f32) -> f32 {
    ((hi.min(x + w) - lo.max(x - w)).max(0.0) / (2.0 * w)).min(1.0)
}

/// A feature of wavelength `lam` at footprint `foot`: 1 while the footprint is under a quarter of it, 0 from a half.
pub(crate) fn lod(lam: f32, foot: f32) -> f32 {
    1.0 - smooth(0.25 * lam, 0.5 * lam, foot)
}

/// The share of a footprint `foot` wide round `e` that lies where `e` is negative (a box filter of the step), so a
/// line thinner than the footprint still shows, as faint as the share it covers; the area mean `mean` once the
/// pattern's spacing `spacing` is unresolved (where lines a footprint apart would beat).
pub(crate) fn edge(e: f32, foot: f32, spacing: f32, mean: f32) -> f32 {
    let f = foot.max(2.0e-4);
    let k = lod(spacing, foot);
    k * (0.5 - e / f).clamp(0.0, 1.0) + (1.0 - k) * mean
}

/// The gradients of `gn`: 16 unit vectors evenly round the circle (a corner's hash picks one, with no trigonometry
/// per lookup).
pub(crate) const GRADIENTS: [(f32, f32); 16] = [
    (1.0, 0.0),
    (0.92388, 0.382683),
    (0.707107, 0.707107),
    (0.382683, 0.92388),
    (0.0, 1.0),
    (-0.382683, 0.92388),
    (-0.707107, 0.707107),
    (-0.92388, 0.382683),
    (-1.0, 0.0),
    (-0.92388, -0.382683),
    (-0.707107, -0.707107),
    (-0.382683, -0.92388),
    (0.0, -1.0),
    (0.382683, -0.92388),
    (0.707107, -0.707107),
    (0.92388, -0.382683),
];

/// Gradient noise, about -1..1, cells of 1.
pub(crate) fn gn(x: f32, y: f32, seed: u32) -> f32 {
    let (ix, iy) = (x.floor(), y.floor());
    let (fx, fy) = (x - ix, y - iy);
    let (ix, iy) = (ix as i32, iy as i32);
    let g = |i: i32, j: i32, dx: f32, dy: f32| {
        let (gx, gy) = GRADIENTS[(hash3_u32(i, j, 0, seed) >> 28) as usize];
        gx * dx + gy * dy
    };
    let f = |t: f32| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    let (ux, uy) = (f(fx), f(fy));
    let a = g(ix, iy, fx, fy);
    let b = g(ix + 1, iy, fx - 1.0, fy);
    let c = g(ix, iy + 1, fx, fy - 1.0);
    let d = g(ix + 1, iy + 1, fx - 1.0, fy - 1.0);
    1.4 * ((a + (b - a) * ux) + ((c + (d - c) * ux) - (a + (b - a) * ux)) * uy)
}

/// Gradient noise of wavelength `lam`, faded to 0 (its mean) as the footprint passes it.
pub(crate) fn gl(x: f32, y: f32, lam: f32, foot: f32, seed: u32) -> f32 {
    let k = lod(lam, foot);
    if k <= 0.0 { 0.0 } else { k * gn(x / lam, y / lam, seed) }
}

/// Band-limited FBM of gradient noise from wavelength `lam` down by halves, `n` octaves, about -1..1 (its spread
/// about 0.2 for 4 or 5 octaves: a threshold at 0.4 leaves about 2 % of a surface over it, 0.25 about 10 %, 0.1 a third:
/// the wear thresholds below are set from that). The weights do
/// not renormalise as octaves fade, so a far surface keeps the large octaves' contrast and loses only the fine ones.
pub(crate) fn fbm(x: f32, y: f32, lam: f32, n: u32, foot: f32, seed: u32) -> f32 {
    let (mut s, mut a, mut l, mut w) = (0.0, 0.5, lam, 0.0);
    for i in 0..n {
        w += a;
        let k = lod(l, foot);
        if k > 0.0 {
            s += a * k * gn(x / l + 17.3 * i as f32, y / l - 9.1 * i as f32, seed.wrapping_add(i * 1013));
        }
        a *= 0.5;
        l *= 0.5;
    }
    if w > 0.0 { s / w } else { 0.0 }
}

/// A uniform 0..1 from a hash and a key.
pub(crate) fn h01(h: u32, k: u32) -> f32 {
    (hash3_u32(h as i32, k as i32, 3, 0x51ed) >> 8) as f32 / 16777216.0
}

/// The spread (standard deviation) of `gn`.
pub(crate) const GN_SPREAD: f32 = 0.30;

/// The share of a footprint where a band-limited noise field `f` passes the threshold `t`: its resolved part against
/// the threshold, softened by the variance `lost` its faded parts no longer carry (a smooth step 1.9 spreads to either
/// side has a normal distribution's slope), so streaks seen from afar are their expected coverage, never nothing and
/// never shimmer.
pub(crate) fn over(f: f32, t: f32, lost: f32) -> f32 {
    let w = (4.0e-4 + 3.6 * lost).sqrt();
    smooth(t - w, t + w, f)
}

/// Water's runs at (u along, d down; m): long thin streaks (1.2 cm across, 60 cm long), speckles (8 mm), broad runs
/// (5 cm across, 1 m long) and the columns they gather in (15 cm across, 2 m long). (the field, about -1..1; the variance of its parts the footprint no longer resolves)
pub(crate) fn runs(u: f32, d: f32, foot: f32, seed: u32) -> (f32, f32) {
    let (mut f, mut lost) = (0.0, 0.0);
    for (a, lam, stretch, k) in [(0.4f32, 0.012f32, 0.02f32, 1u32), (0.35, 0.008, 1.0, 2), (0.4, 0.05, 0.05, 3), (0.5, 0.15, 0.075, 4)] {
        let kk = lod(lam, foot);
        if kk > 0.0 {
            f += a * kk * gn(u / lam + 31.7 * k as f32, d * stretch / lam, seed ^ (k * 977));
        }
        lost += (a * GN_SPREAD) * (a * GN_SPREAD) * (1.0 - kk * kk);
    }
    (f, lost)
}

/// How much of a run's field `f` (variance `lost` unresolved) still marks the wall a share `t` of the way down its
/// length: all of it at the source, fraying through runs and speckles, nothing past its end.
pub(crate) fn hang(t: f32, f: f32, lost: f32) -> f32 {
    if t >= 1.3 {
        return 0.0;
    }
    let th = -0.6 + 1.2 * t.max(0.0);
    over(f, th, lost) * (1.0 - 0.5 * t.min(1.0))
}

/// Drip runs below a curtain at (u along, d down; m): in columns 4 cm apart a share `share` of them run, each from a
/// point within the first `start` m of its source for its own length (up to `reach`), a tail 2-4 mm wide thickening to
/// a head (a drop 6-14 mm across) at its lower end. Lines thinner than the footprint keep their share of it, and the
/// columns fade to their mean once they are unresolved. (coverage 0..1)
pub(crate) fn drips(u: f32, d: f32, foot: f32, start: f32, reach: f32, share: f32, seed: u32) -> f32 {
    const COL: f32 = 0.04;
    let ci = (u / COL).floor();
    let h = hash3_u32(ci as i32, 1, 61, seed);
    // (a drip's area over its column's span: its tail's width over the column, half the span on average)
    let mean = share * 0.5 * (0.003 / COL) * (1.0 - smooth(0.5 * (start + reach), start + reach, d));
    if lod(COL, foot) <= 0.0 {
        return mean;
    }
    if h01(h, 1) >= share {
        return edge(1.0, foot, COL, mean);
    }
    let s0 = start * (0.2 + 0.8 * h01(h, 2));
    let len = 0.06 + reach * 0.45 * h01(h, 3);
    let x = u - ci * COL - (0.25 + 0.5 * h01(h, 4)) * COL;
    let dd = d - s0;
    let rx = 0.003 + 0.004 * h01(h, 5);
    let ry = 1.6 * rx;
    let tail = if (0.0..=len).contains(&dd) { x.abs() - (0.0008 + 0.0012 * dd / len) } else { 1.0 };
    let head = (((x / rx) * (x / rx) + ((dd - len) / ry) * ((dd - len) / ry)).sqrt() - 1.0) * rx;
    edge(tail.min(head), foot, COL, mean)
}

/// How far below its source the lowest drip `drips(.., start, reach, ..)` draws can end (m): the latest start, the
/// longest run, its head's half height (1.6 x 7 mm) and half the widest footprint the drips are resolved at (2 cm).
pub(crate) fn drips_end(start: f32, reach: f32) -> f32 {
    start + 0.06 + 0.45 * reach + 0.0112 + 0.01
}

/// The streaks hanging from a line source (a wall's top, a joint, a deck's edge) at (u along it, d down from it; m): wet
/// stretches about `cell` long with dry gaps between (wet over a share `share` of the source), each reaching its own
/// length (up to `reach`) with a lower edge in tongues of different lengths and its own strength, dense at the source
/// and fraying downward into runs and speckles, the water wandering a little in the wall's own relief; some runs go on as
/// drips (`drip`: on a wall, not under a ceiling). (coverage 0..1, how near its source 0..1)
pub(crate) fn curtain(u: f32, d: f32, foot: f32, cell: f32, reach: f32, share: f32, drip: bool, seed: u32) -> (f32, f32) {
    // (the runs end by 1.3 x the reach; the drips, from 0.8 of a run's length, can end below that on a short reach)
    let end = if drip { (1.3 * reach).max(drips_end(0.8 * reach, reach)) } else { 1.3 * reach };
    if d < 0.0 || d > end || share <= 0.0 {
        return (0.0, 0.0);
    }
    let uu = u + 0.03 * (d / 0.5).min(1.0) * gl(u, d, 0.3, foot, seed ^ 1);
    // how far the water runs here: stretches and gaps at two cells, tongues at 25 cm
    let g = 0.55 * gl(uu, 0.37, 2.0 * cell, foot, seed ^ 3)
        + 0.35 * gl(uu, 0.61, 0.6 * cell, foot, seed ^ 4)
        + 0.3 * gl(uu, 0.83, 0.25, foot, seed ^ 5);
    // (a few runs go on further below the rest)
    let long = smooth(0.1, 0.45, gl(uu, 0.29, 0.4, foot, seed ^ 9));
    let len = reach * (smooth(-0.35, 0.55, share - 0.5 + g) * (0.75 + 0.6 * long)).min(1.0);
    let strength = 0.65 + 0.35 * smooth(-0.3, 0.3, gl(uu, 0.47, 3.0 * cell, foot, seed ^ 8) + 0.5 * g);
    let lip = 0.08 * reach + 0.05;
    // (below its runs and its lip only the drips are left: the runs' field is not needed there)
    let (streaks, film) = if d < 1.3 * len || d < 1.3 * lip {
        let (f, lost) = runs(uu, d, foot, seed ^ 2);
        // the runs, over a soft stain thinning down the length (so a tongue's end fades rather than stops)
        let streaks = if len > 1.0e-3 {
            let t = d / len;
            strength * hang(t, f, lost).max(0.5 * (1.0 - t).max(0.0).powi(2))
        } else {
            0.0
        };
        // a thin dark lip along the whole source
        (streaks, 0.85 * share.sqrt() * hang(d / lip, f, lost))
    } else {
        (0.0, 0.0)
    };
    let drip = if drip { drips(uu, d, foot, 0.8 * len, reach, 0.25 * strength * smooth(0.0, 0.2, len / reach), seed ^ 7) } else { 0.0 };
    (streaks.max(drip).max(film).min(1.0), 1.0 - (d / reach).min(1.0))
}

/// Runs from holes on a grid `pitch` apart, the first `off` in along and up (tie holes, bolts), under the wall's top
/// `top` (m up): a share `share` of them leak, each a narrow streak widening down from it, up to `reach` long.
/// (coverage 0..1, how near its hole 0..1)
pub(crate) fn from_holes(u: f32, v: f32, top: f32, foot: f32, pitch: f32, off: f32, reach: f32, share: f32, seed: u32) -> (f32, f32) {
    let col = ((u - off) / pitch).round();
    let x = u - (col * pitch + off);
    let k = lod(pitch, foot);
    // (a leaking hole's streak at its mean: about 3 cm wide over the column, half its length)
    let mean = share * 0.5 * (0.03 / pitch);
    if k <= 0.0 {
        return (mean, 0.0);
    }
    let (mut c, mut near) = (0.0f32, 0.0f32);
    if x.abs() < 0.08 {
        let row0 = ((v - off) / pitch).ceil();
        for j in 0..3 {
            let vh = (row0 + j as f32) * pitch + off;
            let d = vh - 0.0125 - v;
            if vh > top - 0.1 || d > 1.3 * reach {
                break;
            }
            let h = hash3_u32(col as i32, (row0 as i32) + j, 62, seed);
            if d < 0.0 || h01(h, 1) >= share {
                continue;
            }
            let len = reach * (0.25 + 0.75 * h01(h, 2));
            let hw = 0.006 + 0.008 * h01(h, 3) + 0.018 * (d / len).min(1.3);
            let xx = x + 0.006 * (d / 0.2).min(1.0) * gl(u, d, 0.15, foot, seed ^ 8);
            let across = (1.0 - smooth(0.35 * hw, hw + 0.5 * foot, xx.abs())) * (1.4 * hw / (hw + foot)).min(1.0);
            let (f, lost) = runs(xx, d, foot, h);
            let streak = (0.6 + 0.4 * h01(h, 4)) * across * hang(d / len, f, lost);
            let drip = drips(u + 0.013, d, foot, 0.01, len, 0.5 * across, h);
            let s = streak.max(drip);
            if s > c {
                c = s;
                near = 1.0 - (d / len).min(1.0);
            }
        }
    }
    (k * c + (1.0 - k) * mean, near * k)
}

/// a + (b - a) t per channel.
pub(crate) fn mix3(a: Vec3f, b: Vec3f, t: f32) -> Vec3f {
    a + (b - a) * t
}

/// The product per channel.
pub(crate) fn mul3(a: Vec3f, b: Vec3f) -> Vec3f {
    vec3f(a.x * b.x, a.y * b.y, a.z * b.z)
}

// ---------------------------------------------------------------------------------------------------------------------
// Wear: grime, dust, kick marks, stains, leaks. Each gives a mask (0..1); the caller picks the colours.

/// Grunge: patches of more and less dirt, 0..1, FBM of `lam` m (the room's 0.4 m) thresholded softly.
pub(crate) fn grunge(u: f32, v: f32, lam: f32, foot: f32, seed: u32) -> f32 {
    smooth(-0.35, 0.35, fbm(u, v, lam, 5, foot, seed))
}

/// Grime and dust along a seam (a floor where it meets a wall) `seam_m` m away: grime in a band 3 to 12 cm wide, its
/// width wandering over 0.7 m, darker where it is narrow, and a lighter fluff of dust 35 cm beyond it; both weighted
/// by `grunge`. (grime, dust)
pub(crate) fn seam_grime(u: f32, v: f32, seam_m: f32, grunge: f32, seed: u32) -> (f32, f32) {
    let r = 0.03 + 0.09 * smooth(-0.6, 0.6, gn(u / 0.7, v / 0.7, seed));
    let grime = (1.0 - (seam_m / r).min(1.0)).powi(2) * (0.6 + 0.4 * grunge);
    let dust = (1.0 - smooth(r, r + 0.35, seam_m)) * smooth(0.0, r, seam_m) * (0.4 + 0.6 * grunge);
    (grime, dust)
}

/// The grime band along a wall's foot, `seam_m` m over the floor: 12 cm high, weighted by `grunge`.
pub(crate) fn skirting_band(seam_m: f32, grunge: f32) -> f32 {
    (1.0 - smooth(0.0, 0.12, seam_m)) * (0.5 + 0.5 * grunge)
}

/// Kick marks on a wall under 25 cm (`seam_m` m over the floor): scuffs 18 cm along and 5 cm high where gradient noise
/// passes a threshold, faded to nothing past a 10 cm footprint.
pub(crate) fn kick_marks(u: f32, v: f32, seam_m: f32, foot: f32, seed: u32) -> f32 {
    (1.0 - smooth(0.12, 0.25, seam_m)) * smooth(0.5, 0.85, gn(u / 0.18, v / 0.05, seed)) * lod(0.1, foot)
}

/// A water stain: where an FBM of 1.6 m passes a threshold that falls as `amount` (0..1) rises, the stain (0..1),
/// and its tide line just inside the edge, faded past a 4 cm footprint. (stain, tide)
pub(crate) fn water_stain(u: f32, v: f32, foot: f32, amount: f32, seed: u32) -> (f32, f32) {
    let f = fbm(u, v, 1.6, 5, foot, seed);
    let t = 0.42 - 0.25 * amount;
    let s = smooth(t, t + 0.06, f);
    let tide = (smooth(t - 0.05, t + 0.01, f) - smooth(t + 0.03, t + 0.11, f)).max(0.0) * lod(0.04, foot);
    (s, tide)
}

/// Leaks from a seam above (a wall's top, `top_m` m below it): columns 22 cm wide wandering along it, a share
/// `0.35 amount` of them leaking, each its own length (0.3 to 2.1 m) and width, fading down. None between leaks,
/// else the leak's weight (0..1; never under 0.3 of it past a 6 cm footprint).
pub(crate) fn leak_columns(u: f32, top_m: f32, foot: f32, amount: f32, seed: u32) -> Option<f32> {
    let cw = 0.22;
    let ci = (u / cw + 0.3 * gn(u / 0.9, 0.0, seed ^ 7)).floor();
    let lh = hash3_u32(ci as i32, 0, 8, seed);
    if h01(lh, 1) < 0.35 * amount {
        let len = 0.3 + 1.8 * h01(lh, 2);
        let x = (u / cw + 0.3 * gn(u / 0.9, 0.0, seed ^ 7)) - ci - 0.5;
        let narrow = 0.15 + 0.25 * h01(lh, 3) + 0.08 * gn(top_m / 0.2, ci, seed ^ 9);
        let across = 1.0 - smooth(narrow * 0.5, narrow, x.abs());
        let down = 1.0 - smooth(0.2 * len, len, top_m);
        Some(across * down * lod(0.06, foot).max(0.3))
    } else {
        None
    }
}

/// A wet stain over `c` with coverage `cov` (a curtain's or a band's): grime that darkens it, the more `near` (0..1)
/// its source and a little warm, turning to green and brown moss by `moss` (0..1) where light and rain reach; (u, d)
/// m along and away from the source, seen at `foot`.
pub(crate) fn wet_stain(c: Vec3f, cov: f32, near: f32, moss: f32, u: f32, d: f32, foot: f32, seed: u32) -> Vec3f {
    if cov <= 0.0 {
        return c;
    }
    // (grime darkens the wall, the more where it is thickest, near its source, and a little warm)
    let grime = mul3(c, vec3f(1.0, 0.98, 0.94) * (0.6 - 0.22 * near));
    if moss <= 0.0 {
        return mix3(c, grime, 0.85 * cov);
    }
    let brown = smooth(-0.35, 0.35, gl(u, d * 0.3, 0.4, foot, seed ^ 70));
    let mc = mix3(vec3f(0.12, 0.15, 0.06), vec3f(0.14, 0.115, 0.075), brown) * (1.0 + 0.25 * gl(u, d, 0.01, foot, seed ^ 71));
    mix3(c, mix3(grime, mc, moss), 0.85 * cov)
}

// ---------------------------------------------------------------------------------------------------------------------
// Cloth and foil (the owner's fold recipe: every fold from the tension map; see the fabric-tension-folds skill). Each
// takes its caller's seeds and offsets, so every caller keeps its own folds.

/// The profile of the tension folds a pull point fans out (0..1, 1 on a fold's crest), at `along` (out from the pull
/// point) and `across` (the angle off its pull, both scaled to the folds' size): warped value-noise ridges, `sharp` of
/// them a ridge and the rest the noise itself. `off` are the two warps' offsets (along, across; along, across), `shift`
/// is added to both along offsets after them (a second fan's own), `seeds` the two warps' and the folds'. The caller
/// weights it by its own reach and distance.
pub(crate) fn fan_folds(along: f32, across: f32, warp: f32, across_scale: f32, sharp: f32, off: [f32; 4], shift: f32, seeds: [u32; 3]) -> f32 {
    let wa = fbm2(along * 0.6 + off[0] + shift, across * 0.15 + off[1], 3, seeds[0]);
    let wb = fbm2(along * 0.6 + off[2] + shift, across * 0.15 + off[3], 3, seeds[1]);
    let ua = along + warp * (wa - 0.5) * 4.0;
    let ub = across + warp * (wb - 0.5) * 4.0 * across_scale * 0.3;
    let v = ((fbm2(ua, ub, 3, seeds[2]) - 0.5) * 1.6 + 0.5).clamp(0.0, 1.0);
    let ridge = 1.0 - (v * 2.0 - 1.0).abs();
    sharp * ridge + (1.0 - sharp) * v
}

/// Patches (0..1) where a cloth's fine crinkle shows: value fbm at `freq` per unit thresholded softly from `lo` to `hi`.
pub(crate) fn patches(x: f32, y: f32, freq: f32, lo: f32, hi: f32, octaves: u32, seed: u32) -> f32 {
    smooth(lo, hi, fbm2(x * freq + 4.4, y * freq + 1.1, octaves, seed))
}

/// Broad soft folds about 0 (-0.5..0.5) at (x, y) already scaled to their size: value fbm of three octaves warped
/// along its diagonal by a two-octave fbm at `warp_freq` of their frequency.
pub(crate) fn broad_folds(x: f32, y: f32, warp_freq: f32, warp_seed: u32, seed: u32) -> f32 {
    let warp = fbm2(x * warp_freq + 5.5, y * warp_freq + 2.1, 2, warp_seed) - 0.5;
    fbm2(x + warp, y - warp, 3, seed) - 0.5
}
