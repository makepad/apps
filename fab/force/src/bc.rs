//! Fixtures and loads on picked regions, made into what the solver takes: which node axes are
//! held and each node's force.
//!
//! - A fixture holds, of each cell its region's quads lie in, the nodes nearest the surface
//!   (within half a cell of the quad's plane, on either side): one layer of nodes at the face,
//!   at any cell size and wherever the face crosses its cells. (Holding every node out to half
//!   a cell inward held two layers wherever the face lay in a cell's inner half: a clamp up
//!   to a cell thick, stiffer than the face, and the demo bracket's front face at Fine held
//!   18374 nodes where one layer is about 9200.)
//! - A load spreads over its region by area: each quad's share goes to the nodes of its cell
//!   by the shape functions at the quad's centre, so the total is exact.
//! - A fixture holds its region rigid, so the edge of the hold concentrates stress no real
//!   support would (a bolt's head and the face it is bolted to give a little), and finer cells
//!   find more of it without end. The cells within its [`reach`] are [`near`] it: the safety
//!   factor is read there apart.

use crate::element::{shape_at, V3};
use crate::grid::Grid;
use crate::surface::{Region, Surface};
use std::collections::HashMap;

/// What a picked region carries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// Held in place (a clamped face, a bolted hole).
    Fixed,
    /// A total force (N, printer frame) spread over the region by area.
    Force(V3),
    /// A pressure (MPa) pushing into the surface.
    Pressure(f64),
    /// A torque (N mm) about the region's axis (a hole's, else its mean normal through its
    /// centroid), right-handed.
    Torque(f64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Condition {
    pub kind: Kind,
    pub region: Region,
}

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// The held axes per node (fixtures hold all three) and each node's force (N).
pub fn nodes(grid: &Grid, surface: &Surface, conditions: &[Condition]) -> (Vec<u8>, Vec<V3>) {
    let mut held = vec![0u8; grid.nodes()];
    let mut force = vec![[0.0; 3]; grid.nodes()];
    for cond in conditions {
        let r = &cond.region;
        let quads = r.faces.iter().map(|&q| {
            let c = surface.quad_center[q as usize].map(|v| v as f64);
            let n = surface.quad_normal[q as usize].map(|v| v as f64);
            (c, n, surface.quad_area[q as usize] as f64)
        });
        match cond.kind {
            Kind::Fixed => {
                for (c, n, _) in quads {
                    let (cell, _) = grid.locate(c);
                    for node in grid.cell_nodes(cell) {
                        if dot(sub(grid.node_pos(node), c), n).abs() <= 0.5 * grid.h[0] {
                            held[node] = crate::solve::HELD;
                        }
                    }
                }
            }
            Kind::Force(f) => {
                if r.area > 0.0 {
                    for (c, _, a) in quads {
                        spread(grid, &mut force, c, f.map(|v| v * a / r.area));
                    }
                }
            }
            Kind::Pressure(p) => {
                for (c, n, a) in quads {
                    spread(grid, &mut force, c, n.map(|v| -p * a * v));
                }
            }
            Kind::Torque(t) => {
                let (centre, axis) = match r.cylinder {
                    Some((c, a, _)) => (c, a),
                    None => (r.centroid, r.normal),
                };
                let arm = |c: V3| {
                    let d = sub(c, centre);
                    sub(d, axis.map(|v| v * dot(d, axis)))
                };
                let sum: f64 = r.faces.iter().map(|&q| {
                    let c = surface.quad_center[q as usize].map(|v| v as f64);
                    let l = arm(c);
                    surface.quad_area[q as usize] as f64 * dot(l, l)
                }).sum();
                if sum > 0.0 {
                    for (c, _, a) in quads {
                        let f = cross(axis, arm(c)).map(|v| v * t * a / sum);
                        spread(grid, &mut force, c, f);
                    }
                }
            }
        }
    }
    (held, force)
}

/// How far past its region a fixture's hold shows in the stress (mm): a hole's radius (the
/// bolt's size) from a hole's, else a quarter of the face's side, 0.5 to 3 mm. In mm, not
/// cells: every cell size leaves out the same part.
pub fn reach(r: &Region) -> f64 {
    match r.cylinder {
        Some((_, _, radius)) => radius,
        None => (0.25 * r.area.sqrt()).clamp(0.5, 3.0),
    }
}

/// Per cell: some of it is within a fixture's [`reach`] of the fixture's region.
pub fn near(grid: &Grid, surface: &Surface, conditions: &[Condition]) -> Vec<bool> {
    let mut near = vec![false; grid.cells()];
    for cond in conditions.iter().filter(|c| c.kind == Kind::Fixed) {
        let rho = reach(&cond.region);
        for &q in &cond.region.faces {
            let c = surface.quad_center[q as usize].map(|v| v as f64);
            mark_within(grid, &mut near, c, rho);
        }
    }
    near
}

/// Marks cells with some part of their box within `rho` of `c`.
fn mark_within(grid: &Grid, marks: &mut [bool], c: V3, rho: f64) {
    let span = |a: usize| {
        let at = |v: f64| (((v - grid.origin[a]) / grid.h[a]).floor().max(0.0) as usize).min(grid.n[a] - 1);
        at(c[a] - rho)..=at(c[a] + rho)
    };
    for k in span(2) {
        for j in span(1) {
            for i in span(0) {
                let d2: f64 = [i, j, k].iter().enumerate().map(|(a, &ix)| {
                    let lo = grid.origin[a] + ix as f64 * grid.h[a];
                    (lo - c[a]).max(c[a] - lo - grid.h[a]).max(0.0).powi(2)
                }).sum();
                if d2 <= rho * rho {
                    marks[grid.cell(i, j, k)] = true;
                }
            }
        }
    }
}

/// The sharp inside edge midpoints of `own` within `reach` of one at the other cell size,
/// edges shorter than `min_len` left out ([`Surface::sharp_edges`]; half the finer cell).
pub fn sharp_points(own: &Surface, other: &Surface, reach: f64, min_len: f64) -> Vec<V3> {
    let bucket = |p: V3| p.map(|v| (v / reach).floor() as i64);
    let mut buckets: HashMap<[i64; 3], Vec<V3>> = HashMap::new();
    for p in other.sharp_edges(min_len) {
        buckets.entry(bucket(p)).or_default().push(p);
    }
    let reach2 = reach * reach;
    let neighbors = |i: i64| i.saturating_sub(1)..=i.saturating_add(1);
    own.sharp_edges(min_len).into_iter().filter(|&p| {
        let [x, y, z] = bucket(p);
        for k in neighbors(z) {
            for j in neighbors(y) {
                for i in neighbors(x) {
                    if let Some(points) = buckets.get(&[i, j, k]) {
                        if points.iter().any(|&q| {
                            let d = sub(q, p);
                            dot(d, d) <= reach2
                        }) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }).collect()
}

/// Marks cells within `reach` of an edge midpoint where the inside corner is sharp at both
/// cell sizes. Stress at such a corner can grow without bound as cells get finer. The mask
/// is made once per model, with one pass over the quad edges of each surface.
pub fn sharp(grid: &Grid, own: &Surface, other: &Surface, reach: f64, min_len: f64) -> Vec<bool> {
    let mut marks = vec![false; grid.cells()];
    for p in sharp_points(own, other, reach, min_len) {
        mark_within(grid, &mut marks, p, reach);
    }
    marks
}

fn spread(grid: &Grid, force: &mut [V3], p: V3, f: V3) {
    let (cell, xi) = grid.locate(p);
    let w = shape_at(xi);
    for (i, node) in grid.cell_nodes(cell).into_iter().enumerate() {
        for a in 0..3 {
            force[node][a] += w[i] * f[a];
        }
    }
}
