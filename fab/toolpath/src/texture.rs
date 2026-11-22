//! Outer-wall textures (EXPSLICE §2.4): the outer wall's centreline is
//! displaced along the wall normal by a pattern field, in the toolpath only.
//! The model, its contours and every other wall stay exact.
//!
//! The texture only cuts inward. The island is first inset by the depth A
//! (contour only; holes keep their size unless they are textured too), so
//! the outer wall's nominal line sits A inside and the inner walls and fill
//! follow it. The texture then pushes each point back out by A·(1 − P),
//! P ∈ [0, 1] the pattern depth: peaks reach the true surface, valleys go
//! A deep, and the bead keeps its overlap with the next wall.
//!
//! Two limits make the result printable:
//! - **Z budget.** A bead must overlap the one below by about half its
//!   width. The local amplitude is clamped so that A·|P(z ± h) − P(z)| ≤
//!   0.45 w. The clamp is a pure function of position, so the layers agree.
//! - **Lateral acceleration.** The wiggle's curvature κ caps the feed at
//!   √(a_max / κ) (98th percentile over the wall), so the firmware's
//!   acceleration limit does not smear the pattern.
//!
//! Patterns live in cylindrical surface coordinates about the part's centre
//! (u: periods around, with a whole number of repeats so the seam is
//! invisible; v = z / scale), except fuzzy noise, which is a solid 3D field.

use crate::exp::{Pattern, Texture};
use crate::ExtrusionPath;
use makepad_fab_slicer::geom::*;
use std::f64::consts::{PI, TAU};

/// Where a texture is evaluated: the part's centre and reference radius
/// (fixed per part, so every layer uses the same pattern frame).
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub cx: f64,
    pub cy: f64,
    /// Repeats around the part.
    pub repeats: f64,
}

impl Frame {
    pub fn new(t: &Texture, cx: f64, cy: f64, r_ref: f64) -> Frame {
        let repeats = (TAU * r_ref.max(1.0) / t.scale).round().max(3.0);
        Frame { cx, cy, repeats }
    }
}

fn smooth(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn hash3(x: i64, y: i64, z: i64) -> f64 {
    let mut h = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ (z as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    h ^= h >> 31;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 29;
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// Smooth value noise in [0, 1], unit cell.
fn noise3(x: f64, y: f64, z: f64) -> f64 {
    let (ix, iy, iz) = (x.floor(), y.floor(), z.floor());
    let (fx, fy, fz) = (smooth(x - ix), smooth(y - iy), smooth(z - iz));
    let (ix, iy, iz) = (ix as i64, iy as i64, iz as i64);
    let l = |a: f64, b: f64, t: f64| a + (b - a) * t;
    let c = |dx, dy, dz| hash3(ix + dx, iy + dy, iz + dz);
    l(
        l(l(c(0, 0, 0), c(1, 0, 0), fx), l(c(0, 1, 0), c(1, 1, 0), fx), fy),
        l(l(c(0, 0, 1), c(1, 0, 1), fx), l(c(0, 1, 1), c(1, 1, 1), fx), fy),
        fz,
    )
}

/// The pattern depth P ∈ [0, 1] (1 = deepest) at a wall point.
pub fn depth(t: &Texture, f: &Frame, x: f64, y: f64, z: f64) -> f64 {
    let theta = (y - f.cy).atan2(x - f.cx);
    let u = (theta / TAU + 0.5) * f.repeats;
    let v = z / t.scale;
    let frac = |a: f64| a - a.floor();
    match t.pattern {
        Pattern::Fuzzy => {
            let s = t.scale;
            // Two octaves: cells of `scale` and half of it.
            let n = 0.7 * noise3(x / s, y / s, z / s) + 0.3 * noise3(2.0 * x / s + 17.0, 2.0 * y / s, 2.0 * z / s);
            smooth((n - 0.15) / 0.7)
        }
        Pattern::Knurl => {
            // Grooves along both helices u ± v; P = 1 in a groove.
            let g = |a: f64| (PI * a).cos().powi(2).powi(3);
            g(u + v).max(g(u - v))
        }
        Pattern::Scales => {
            // Staggered rows; each scale is a cup whose rim is the deep line.
            let row = v.floor();
            let fu = frac(u + 0.5 * (row.rem_euclid(2.0)));
            let fz = v - row;
            let r = ((2.0 * (fu - 0.5)).powi(2) + (1.0 - fz).powi(2)).sqrt();
            smooth((r - 0.7) / 0.3)
        }
        Pattern::Woven => {
            // Checkerboard of strands, alternating direction, with a groove
            // between strands.
            let (cu, cv) = (u.floor() as i64, v.floor() as i64);
            let (fu, fv) = (frac(u), frac(v));
            let across = if (cu + cv).rem_euclid(2) == 0 { fv } else { fu };
            let strand = (PI * across).sin(); // 0 at the strand edges, 1 in the middle
            let edge_u = (PI * fu).sin();
            let edge_v = (PI * fv).sin();
            (1.0 - 0.75 * strand * edge_u.min(edge_v).sqrt()).clamp(0.0, 1.0)
        }
        Pattern::WoodGrain => {
            // Thin, slowly meandering vertical lines with an uneven spacing.
            let wobble = 0.6 * noise3(u * 0.35, v * 0.12, 3.0) + 0.25 * noise3(u * 1.1, v * 0.5, 9.0);
            let line = frac(u + wobble);
            let d = (line - 0.5).abs() * 2.0; // 1 at the line centre
            smooth((d - 0.7) / 0.3) * (0.6 + 0.4 * noise3(u * 0.2, v * 0.05, 5.0))
        }
    }
}

/// True when this path is a hole wall (clockwise ring).
fn is_hole(path: &ExtrusionPath) -> bool {
    let ring = if path.loop_pts.len() > 2 { &path.loop_pts } else { &path.pts };
    Polygon::new(ring.clone()).area2() < 0
}

/// Inset the island's outer contour (and, when textured, its holes) by the
/// texture depth, so the walls inside start below the deepest valley.
pub fn inset_island(island: &ExPolygon, t: &Texture) -> Vec<ExPolygon> {
    use makepad_fab_slicer::clip::intersection_ex;
    use makepad_fab_slicer::offset::{offset_ex, DEFAULT_MITER};
    let d = -t.depth * SCALE;
    if t.holes {
        return offset_ex(std::slice::from_ref(island), d, DEFAULT_MITER);
    }
    let outer = offset_ex(&[ExPolygon::new(island.contour.clone())], d, DEFAULT_MITER);
    intersection_ex(&outer, std::slice::from_ref(island))
}

/// Resample a closed or open polyline at ≤ `step` mm, keeping its vertices:
/// (x, y, segment, mm along the polyline).
fn resample(pts: &[Pt], step: f64) -> Vec<(f64, f64, usize, f64)> {
    let mut out = Vec::with_capacity(pts.len() * 4);
    let mut along = 0.0;
    for i in 0..pts.len().saturating_sub(1) {
        let (ax, ay) = (pts[i].x_mm(), pts[i].y_mm());
        let (bx, by) = (pts[i + 1].x_mm(), pts[i + 1].y_mm());
        let len = (bx - ax).hypot(by - ay);
        let k = (len / step).ceil().max(1.0) as usize;
        // Positions measure as the overhang labels do (unit distances).
        let seg = pts[i].dist(pts[i + 1]) / SCALE;
        for j in 0..k {
            let t = j as f64 / k as f64;
            out.push((ax + (bx - ax) * t, ay + (by - ay) * t, i, along + seg * t));
        }
        along += seg;
    }
    if let Some(p) = pts.last() {
        out.push((p.x_mm(), p.y_mm(), pts.len().saturating_sub(2), along));
    }
    out
}

/// Texture one outer-wall path in place (the path was planned on the inset
/// island). `z` is the layer's print z, `h` its height. When it moves the
/// points, each new point's position (mm along the path as it was, from its
/// first point), so overhang labels made on the line still find them.
pub fn apply(path: &mut ExtrusionPath, z: f64, h: f64, t: &Texture, f: &Frame) -> Option<Vec<f64>> {
    if path.role != crate::Role::OuterWall || path.pts.len() < 2 || !path.z.is_empty() {
        return None;
    }
    if !t.holes && is_hole(path) {
        return None;
    }
    let w = if path.width > 0.0 { path.width } else { 0.45 };
    let budget = 0.45 * w;
    let samples = resample(&path.pts, t.step);
    let n = samples.len();
    // Segment normals (right of travel = away from material for contours
    // and holes alike).
    let seg_n: Vec<(f64, f64)> = (0..path.pts.len().saturating_sub(1))
        .map(|i| {
            let (dx, dy) = (path.pts[i + 1].x_mm() - path.pts[i].x_mm(), path.pts[i + 1].y_mm() - path.pts[i].y_mm());
            let l = dx.hypot(dy).max(1e-12);
            (dy / l, -dx / l)
        })
        .collect();
    let mut out: Vec<Pt> = Vec::with_capacity(n);
    let mut along: Vec<f64> = Vec::with_capacity(n);
    // A variable-width bead: each new segment keeps the flow of the one it lies on.
    let mut flow: Vec<f64> = Vec::new();
    for (k, &(x, y, si, s)) in samples.iter().enumerate() {
        // At an original vertex, the bisector of the two segment normals.
        let mut nrm = seg_n[si.min(seg_n.len() - 1)];
        let at_vertex = k > 0 && samples[k - 1].2 != si;
        if at_vertex && si > 0 {
            let a = seg_n[si - 1];
            let (bx, by) = (a.0 + nrm.0, a.1 + nrm.1);
            let l = bx.hypot(by);
            if l > 1e-6 {
                nrm = (bx / l, by / l);
            }
        }
        let p = depth(t, f, x, y, z);
        let dp = (depth(t, f, x, y, z + h) - p).abs().max((depth(t, f, x, y, z - h) - p).abs());
        let a = if dp * t.depth > budget { budget / dp } else { t.depth };
        let o = t.depth - a * p;
        let q = Pt::from_mm(x + nrm.0 * o, y + nrm.1 * o);
        if out.last() == Some(&q) {
            continue;
        }
        if !out.is_empty() && !path.flow.is_empty() {
            flow.push(path.flow[samples[k - 1].2.min(path.flow.len() - 1)]);
        }
        out.push(q);
        along.push(s);
    }
    let moved = out.len() >= 2;
    if moved {
        path.pts = out;
        path.flow = flow;
        path.mode = crate::exp::mode::TEXTURE;
    }
    // The wipe ring follows the textured line.
    if path.closed {
        path.loop_pts = path.pts.clone();
    }
    moved.then_some(along)
}

/// The feed cap (mm/s) of a textured wall: √(a_max / κ) at the wall's 98th
/// percentile curvature. None when the wall is not textured enough to matter.
pub fn speed_cap(path: &ExtrusionPath, t: &Texture) -> Option<f64> {
    let p = &path.pts;
    if p.len() < 3 {
        return None;
    }
    let mut ks: Vec<f64> = p
        .windows(3)
        .filter_map(|w| {
            let (ax, ay) = (w[1].x_mm() - w[0].x_mm(), w[1].y_mm() - w[0].y_mm());
            let (bx, by) = (w[2].x_mm() - w[1].x_mm(), w[2].y_mm() - w[1].y_mm());
            let (la, lb) = (ax.hypot(ay), bx.hypot(by));
            if la < 1e-6 || lb < 1e-6 {
                return None;
            }
            let turn = (ax * by - ay * bx).atan2(ax * bx + ay * by).abs();
            Some(turn / (0.5 * (la + lb)))
        })
        .collect();
    if ks.is_empty() {
        return None;
    }
    ks.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let k98 = ks[((ks.len() - 1) as f64 * 0.98) as usize];
    (k98 > 1e-6).then(|| (t.max_accel / k98).sqrt().max(10.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tex(p: Pattern) -> Texture {
        Texture { pattern: p, depth: 0.35, scale: 2.0, step: 0.2, holes: false, max_accel: 20_000.0 }
    }

    fn circle(r: f64, n: usize) -> Vec<Pt> {
        let mut v: Vec<Pt> = (0..=n).map(|i| { let a = TAU * i as f64 / n as f64; Pt::from_mm(r * a.cos(), r * a.sin()) }).collect();
        v[n] = v[0];
        v
    }

    fn wall(r: f64) -> ExtrusionPath {
        let pts = circle(r, 400);
        ExtrusionPath { role: crate::Role::OuterWall, loop_pts: pts.clone(), pts, closed: true, width: 0.45, height: 0.2, ..Default::default() }
    }

    #[test]
    fn every_pattern_stays_in_range_and_overlaps_the_layer_below() {
        for pat in [Pattern::Fuzzy, Pattern::Knurl, Pattern::Scales, Pattern::Woven, Pattern::WoodGrain] {
            let t = tex(pat);
            let f = Frame::new(&t, 0.0, 0.0, 20.0);
            let (h, r0) = (0.2, 20.0 - t.depth);
            let radii = |z: f64| -> Vec<f64> {
                let mut w = wall(r0);
                apply(&mut w, z, h, &t, &f);
                w.pts.iter().map(|p| p.x_mm().hypot(p.y_mm())).collect()
            };
            let (mut lo, mut hi, mut worst_shift) = (f64::MAX, 0.0f64, 0.0f64);
            for li in 1..60 {
                let z = li as f64 * h;
                let a = radii(z);
                let b = radii(z + h);
                for &r in &a {
                    lo = lo.min(r);
                    hi = hi.max(r);
                }
                // Same resampling every layer: compare point by point.
                assert_eq!(a.len(), b.len());
                for (ra, rb) in a.iter().zip(&b) {
                    worst_shift = worst_shift.max((ra - rb).abs());
                }
            }
            assert!(hi <= 20.0 + 1e-3, "{pat:?}: a peak beyond the surface ({hi})");
            assert!(lo >= 20.0 - t.depth - 1e-3, "{pat:?}: a valley deeper than the depth ({lo})");
            assert!(hi - lo > 0.1, "{pat:?}: no visible texture ({lo}..{hi})");
            // The clamp is per point and z; one layer's own clamp may differ
            // from the next one's, so allow a little over the budget.
            assert!(worst_shift <= 0.45 * 0.45 * 1.25, "{pat:?}: layer shift {worst_shift}");
        }
    }

    #[test]
    fn hole_walls_stay_untouched_by_default() {
        let t = tex(Pattern::Knurl);
        let f = Frame::new(&t, 0.0, 0.0, 20.0);
        let mut w = wall(5.0);
        w.pts.reverse();
        w.loop_pts.reverse();
        let before = w.pts.clone();
        apply(&mut w, 1.0, 0.2, &t, &f);
        assert_eq!(w.pts, before);
    }

    #[test]
    fn the_seam_is_continuous() {
        let t = tex(Pattern::Knurl);
        let f = Frame::new(&t, 0.0, 0.0, 20.0);
        let mut w = wall(19.65);
        apply(&mut w, 3.0, 0.2, &t, &f);
        let (a, b) = (w.pts[0], *w.pts.last().unwrap());
        assert!(a.dist(b) as f64 / SCALE < 0.25, "seam gap {}", a.dist(b) as f64 / SCALE);
    }

    #[test]
    fn a_wiggly_wall_is_slowed() {
        let t = tex(Pattern::Knurl);
        let f = Frame::new(&t, 0.0, 0.0, 20.0);
        let mut w = wall(19.65);
        apply(&mut w, 3.0, 0.2, &t, &f);
        let cap = speed_cap(&w, &t).unwrap();
        assert!(cap < 400.0 && cap >= 10.0, "cap {cap}");
    }
}
