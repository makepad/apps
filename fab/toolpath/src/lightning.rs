//! Lightning infill: trees that hold up only what lies above them.
//!
//! The object is walked top down. On each layer the demand is the solid
//! area of the layer above that rests on this layer's sparse area; every
//! demand point must lie within `radius` (the line spacing) of a tree.
//! Uncovered points become new leaves, joined to the nearest tree node or
//! to the nearest wall, whichever is closer. Going down, every leaf moves
//! towards its parent by the layer height (a 45° lean), so trees narrow
//! downwards until only their wall anchors remain; nodes that leave the
//! sparse area drop, and their branches re-anchor on the nearest wall.
//!
//! The pass is sequential over layers (each layer's trees come from the
//! one above); its output is one segment list per layer, which the
//! per-layer fill clips to each island.

use crate::clipper::Clipper;
use makepad_fab_slicer::clip::{difference_ex, intersection_ex};
use makepad_fab_slicer::geom::*;

#[derive(Clone, Copy, Debug)]
struct Node {
    p: Pt,
    parent: u32,
}

const ROOT: u32 = u32::MAX;

/// Tree segments per layer (bed units). `sparse[i]` is the sparse area of
/// layer i, `fill[i]` everything inside the walls; `h[i]` the layer heights.
pub fn trees(sparse: &[Vec<ExPolygon>], fill: &[Vec<ExPolygon>], h: &[f64], radius: f64, pivot: Pt) -> Vec<Vec<(Pt, Pt)>> {
    let n = sparse.len();
    let mut out = vec![Vec::new(); n];
    let r = radius * SCALE;
    let mut nodes: Vec<Node> = Vec::new();
    for i in (0..n).rev() {
        if sparse[i].is_empty() {
            nodes.clear();
            continue;
        }
        let clip = Clipper::new(&sparse[i]);
        // 1. Leaves lean in towards their parents; spent leaves drop.
        if !nodes.is_empty() {
            let step = h.get(i + 1).copied().unwrap_or(h[i]) * SCALE;
            nodes = retract(&nodes, step);
        }
        // 2. Nodes outside this layer's sparse area drop; orphans anchor.
        if !nodes.is_empty() {
            nodes = keep_inside(&nodes, &clip, &sparse[i]);
        }
        // 3. Demand from the solid above resting on this sparse area.
        if i + 1 < n && !fill[i + 1].is_empty() {
            let solid_above = difference_ex(&fill[i + 1], &sparse[i + 1]);
            let demand = intersection_ex(&solid_above, &sparse[i]);
            if !demand.is_empty() {
                grow(&mut nodes, &demand, &sparse[i], r, pivot);
            }
        }
        for nd in &nodes {
            if nd.parent != ROOT {
                let q = nodes[nd.parent as usize].p;
                if q != nd.p {
                    out[i].push((nd.p, q));
                }
            }
        }
    }
    out
}

fn retract(nodes: &[Node], step: f64) -> Vec<Node> {
    let mut children = vec![0u32; nodes.len()];
    for nd in nodes {
        if nd.parent != ROOT {
            children[nd.parent as usize] += 1;
        }
    }
    let mut keep = vec![true; nodes.len()];
    let mut moved: Vec<Node> = nodes.to_vec();
    for (k, nd) in nodes.iter().enumerate() {
        if children[k] == 0 && nd.parent != ROOT {
            let q = nodes[nd.parent as usize].p;
            let d = nd.p.dist(q);
            if d <= step {
                keep[k] = false;
            } else {
                moved[k].p = lerp(nd.p, q, step / d);
            }
        } else if children[k] == 0 {
            // A lone root holds nothing.
            keep[k] = false;
        }
    }
    compact(&moved, &keep)
}

/// Remove the nodes not kept; children of removed nodes become roots.
fn compact(nodes: &[Node], keep: &[bool]) -> Vec<Node> {
    let mut map = vec![ROOT; nodes.len()];
    let mut out = Vec::with_capacity(nodes.len());
    for (k, nd) in nodes.iter().enumerate() {
        if keep[k] {
            map[k] = out.len() as u32;
            out.push(*nd);
        }
    }
    for nd in &mut out {
        if nd.parent != ROOT {
            nd.parent = map[nd.parent as usize];
        }
    }
    out
}

fn keep_inside(nodes: &[Node], clip: &Clipper, region: &[ExPolygon]) -> Vec<Node> {
    let keep: Vec<bool> = nodes.iter().map(|nd| clip.contains(nd.p)).collect();
    let mut out = compact(nodes, &keep);
    // Roots that were branches before lost their anchor: anchor them on
    // the nearest wall.
    let was_root: Vec<bool> = nodes.iter().zip(&keep).filter(|(_, k)| **k).map(|(nd, _)| nd.parent == ROOT).collect();
    let mut extra = Vec::new();
    for (k, nd) in out.iter_mut().enumerate() {
        if nd.parent == ROOT && !was_root[k] {
            if let Some(w) = nearest_wall(region, nd.p) {
                if w != nd.p {
                    extra.push((k, w));
                }
            }
        }
    }
    for (k, w) in extra {
        let id = out.len() as u32;
        out.push(Node { p: w, parent: ROOT });
        out[k].parent = id;
    }
    out
}

fn nearest_wall(region: &[ExPolygon], p: Pt) -> Option<Pt> {
    let mut best: Option<(f64, Pt)> = None;
    for r in region.iter().flat_map(|e| e.rings()) {
        let m = r.pts.len();
        for j in 0..m {
            let (a, b) = (r.pts[j], r.pts[(j + 1) % m]);
            let q = project(p, a, b);
            let d = p.dist(q);
            if best.map_or(true, |x| d < x.0) {
                best = Some((d, q));
            }
        }
    }
    best.map(|x| x.1)
}

fn project(p: Pt, a: Pt, b: Pt) -> Pt {
    let ab = b.sub(a);
    let l2 = dot(ab, ab);
    if l2 == 0 {
        return a;
    }
    let t = (dot(p.sub(a), ab) as f64 / l2 as f64).clamp(0.0, 1.0);
    lerp(a, b, t)
}

/// Add leaves for demand points not within `r` of a tree.
fn grow(nodes: &mut Vec<Node>, demand: &[ExPolygon], region: &[ExPolygon], r: f64, pivot: Pt) {
    // Demand samples on a grid of r anchored at the pivot.
    let dc = Clipper::new(demand);
    let bb = dc.bbox;
    let ri = r.max(1.0) as i64;
    let x0 = pivot.x + (bb.min.x - pivot.x).div_euclid(ri) * ri;
    let y0 = pivot.y + (bb.min.y - pivot.y).div_euclid(ri) * ri;
    let mut samples = Vec::new();
    let mut y = y0;
    while y <= bb.max.y {
        let mut x = x0;
        while x <= bb.max.x {
            let p = Pt::new(x, y);
            if dc.contains(p) {
                samples.push(p);
            }
            x += ri;
        }
        y += ri;
    }
    // Small demand areas between grid points still get one sample.
    for e in demand {
        let c = e.bbox().center();
        if dc.contains(c) && samples.iter().all(|s| s.dist(c) > r) {
            samples.push(c);
        }
    }
    for p in samples {
        // Covered by an existing branch?
        let mut covered = false;
        let mut nearest: Option<(f64, u32)> = None;
        for (k, nd) in nodes.iter().enumerate() {
            let d = nd.p.dist(p);
            if nearest.map_or(true, |x| d < x.0) {
                nearest = Some((d, k as u32));
            }
            let q = if nd.parent != ROOT { nodes[nd.parent as usize].p } else { nd.p };
            if dist_point_segment(p, nd.p, q) <= r {
                covered = true;
                break;
            }
        }
        if covered {
            continue;
        }
        let wall = nearest_wall(region, p);
        let to_wall = wall.map_or(f64::INFINITY, |w| w.dist(p));
        let id = nodes.len() as u32;
        match nearest {
            Some((d, k)) if d <= to_wall => nodes.push(Node { p, parent: k }),
            _ => {
                let Some(w) = wall else { continue };
                nodes.push(Node { p, parent: id + 1 });
                nodes.push(Node { p: w, parent: ROOT });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trees_hold_a_top_and_thin_downwards() {
        let sq = |a: f64, b: f64| ExPolygon::new(Polygon::new(vec![Pt::from_mm(a, a), Pt::from_mm(b, a), Pt::from_mm(b, b), Pt::from_mm(a, b)]));
        let n = 40;
        let mut sparse = vec![vec![sq(0.0, 30.0)]; n];
        let fill = vec![vec![sq(0.0, 30.0)]; n];
        sparse[n - 1].clear(); // the top layer is solid
        let h = vec![0.2; n];
        let t = trees(&sparse, &fill, &h, 3.0, Pt::from_mm(15.0, 15.0));
        let len = |l: &Vec<(Pt, Pt)>| l.iter().map(|(a, b)| a.dist(*b)).sum::<f64>() / SCALE;
        assert!(len(&t[n - 2]) > 0.0);
        assert!(len(&t[n - 2]) >= len(&t[n - 20]));
        assert!(len(&t[0]) < len(&t[n - 2]));
        // Deterministic.
        let t2 = trees(&sparse, &fill, &h, 3.0, Pt::from_mm(15.0, 15.0));
        assert_eq!(t, t2);
    }
}
