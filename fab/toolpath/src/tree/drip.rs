//! D29, Fab's, not the reference's: nothing prints on air over the hollow inside of the support
//! under it (R3). Where branches part, a hole opens in a base island over the hollow inside of the
//! branch under it, and the loop round it, with two walls the second wall's loop round that too,
//! prints with none of its bead on a bead under it. Each such path ([`on_air`]) stands on a drip
//! ([`drip`]): on the layers under it, copies of it, each shrunk toward one point of a bead under
//! the lowest, by as much as keeps every point of each within [`DRIP_LEAN`] of a layer's lean of
//! the copy under it (R1). A copy is the path's own shape, so the drip under a loop is a stack of
//! smaller loops that leans to the wall, and leaves the branch's inside open round it.

use std::sync::Arc;

use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::grid::SegGrid;

use super::draw::Bead;
use super::interface::Layers;
use super::settings::Settings;
use super::Input;
use crate::support_rules::closer;
use crate::{ExtrusionPath, Role};

/// The share of a layer's lean a copy of a drip stands off the one under it, at its farthest.
const DRIP_LEAN: f64 = 0.9;

/// The segments of `path`'s centreline, a closed one's back to its start.
fn segments(path: &ExtrusionPath) -> impl Iterator<Item = (Pt, Pt)> + '_ {
    let back = (path.closed && path.pts.len() > 2).then(|| (path.pts[path.pts.len() - 1], path.pts[0]));
    path.pts.windows(2).map(|w| (w[0], w[1])).chain(back)
}

/// The box of segment `a`-`b` grown by `r` (units).
fn around(a: Pt, b: Pt, r: i64) -> BBox {
    BBox { min: Pt::new(a.x.min(b.x) - r, a.y.min(b.y) - r), max: Pt::new(a.x.max(b.x) + r, a.y.max(b.y) + r) }
}

/// The paths of `here`, one layer's, that print on air: none of their bead on a bead of `under`,
/// the layer under it, and every point over `support`, what the support covers there. A path
/// that leaves it stands on the part, or leans out from the support, as the planner keeps it.
pub(super) fn on_air(here: &[ExtrusionPath], under: &[ExtrusionPath], support: &[&[ExPolygon]]) -> Vec<usize> {
    let grid = SegGrid::new(under.iter().enumerate().flat_map(|(k, p)| segments(p).map(move |(a, b)| (a, b, k as u32))).collect());
    let widest = under.iter().map(|p| p.width).fold(0.0, f64::max);
    let touches = |path: &ExtrusionPath| {
        let hw = path.width / 2.0;
        let reach = ((hw + widest / 2.0) * SCALE).ceil() as i64;
        segments(path).any(|(a, b)| {
            let mut on = false;
            grid.visit_box(around(a, b, reach), |c, d, k| on = on || closer(a, b, c, d, ((hw + under[k as usize].width / 2.0) * SCALE).round() as i64));
            on
        })
    };
    let over = |q: Pt| support.iter().flat_map(|s| s.iter()).any(|e| e.contains(q));
    here.iter().enumerate().filter(|(_, path)| segments(path).next().is_some() && !touches(path) && path.pts.iter().all(|&q| over(q))).map(|(i, _)| i).collect()
}

/// The point of segment `a`-`b` nearest `p`.
fn foot(a: Pt, b: Pt, p: Pt) -> Pt {
    let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { ((((p.x - a.x) as f64) * dx + ((p.y - a.y) as f64) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    Pt::new((a.x as f64 + dx * t).round() as i64, (a.y as f64 + dy * t).round() as i64)
}

/// The point of the centrelines of `paths` nearest `p`; none without one.
fn nearest(paths: &[ExtrusionPath], p: Pt) -> Option<Pt> {
    let mut best: Option<(f64, Pt)> = None;
    for (a, b) in paths.iter().flat_map(segments) {
        let q = foot(a, b, p);
        let d = q.dist(p);
        if best.is_none_or(|(e, _)| d < e) {
            best = Some((d, q));
        }
    }
    best.map(|(_, q)| q)
}

/// The drip under `path`, which prints on air on layer `l` ([`on_air`]), as the points of its
/// copies, the first for layer l − 1 and on down; `out` every layer's paths, `lean` each layer's
/// lean (units). The fewest copies k such that p, the point of a bead of layer l − k − 1 nearest
/// the middle of the path's box, lies within k + 1 steps of every point of the path, a step
/// [`DRIP_LEAN`] of the least lean of the layers from l − k to l: the t-th copy is the path
/// shrunk toward p to k + 1 − t of k + 1, so each lies within a step of the one under it and the
/// last within one of p. None where no k does, or a copy would leave what the support covers on
/// its layer, as `inside` tells of each point.
fn drip(path: &ExtrusionPath, l: usize, out: &[Arc<Vec<ExtrusionPath>>], lean: &[f64], inside: &impl Fn(usize, Pt) -> bool) -> Option<Vec<Vec<Pt>>> {
    let middle = BBox::of(&path.pts).center();
    for k in 1..l {
        let Some(p) = nearest(&out[l - k - 1], middle) else { continue };
        let step = DRIP_LEAN * lean[l - k..=l].iter().copied().fold(f64::INFINITY, f64::min);
        let far = path.pts.iter().map(|q| q.dist(p)).fold(0.0, f64::max);
        if far > step * (k + 1) as f64 {
            continue;
        }
        let toward = |q: Pt, f: f64| Pt::new((p.x as f64 + (q.x - p.x) as f64 * f).round() as i64, (p.y as f64 + (q.y - p.y) as f64 * f).round() as i64);
        let copies: Vec<Vec<Pt>> = (1..=k).map(|t| path.pts.iter().map(|&q| toward(q, (k + 1 - t) as f64 / (k + 1) as f64)).collect()).collect();
        return copies.iter().enumerate().all(|(t, c)| c.iter().all(|&q| inside(l - 1 - t, q))).then_some(copies);
    }
    None
}

/// How far `p` lies from the nearest ring of `e` (units).
fn off_edge(e: &ExPolygon, p: Pt) -> f64 {
    e.rings().flat_map(|r| (0..r.pts.len()).map(move |i| (r.pts[i], r.pts[(i + 1) % r.pts.len()]))).map(|(a, b)| dist_point_segment(p, a, b)).fold(f64::INFINITY, f64::min)
}

/// The drips (D29) under the paths `air` names, each (layer, index in `out[layer]`), as the
/// layers under them print them: per layer, its copies in the order of the paths they stand
/// under, each in the support line at its layer's height. A copy keeps to where support may print
/// on its layer: each point in its base or interface, or with a bead there the side gap off the
/// part, as where branches meet round a pocket the layers under them leave open.
pub(super) fn drips(s: &Settings, input: &Input, layers: &Layers, out: &[Arc<Vec<ExtrusionPath>>], air: &[(usize, usize)]) -> Vec<Vec<ExtrusionPath>> {
    let n = out.len();
    let mut got = vec![Vec::new(); n];
    if air.is_empty() {
        return got;
    }
    let tan = s.keys.branch_angle_deg.to_radians().tan();
    let lean: Vec<f64> = (0..n).map(|i| if i == 0 { 0.0 } else { (input.heights[i].0 - input.heights[i - 1].0).max(0.0) * tan * SCALE }).collect();
    let gap = (s.keys.xy_distance + s.keys.line_width / 2.0) * SCALE;
    let inside = |i: usize, q: Pt| layers.base[i].iter().chain(&layers.interface[i]).any(|e| e.contains(q)) || !input.slices[i].iter().any(|e| e.contains(q) || off_edge(e, q) < gap);
    for &(l, k) in air {
        let path = &out[l][k];
        for (t, pts) in drip(path, l, out, &lean, &inside).into_iter().flatten().enumerate() {
            let i = l - 1 - t;
            let bead = Bead { w: path.width, h: input.heights[i].1, speed: input.print.speed };
            got[i].push(if path.closed { bead.closed(Role::Support, pts) } else { bead.open(Role::Support, pts) });
        }
    }
    got
}
