//! Support points: where the supports touch the part, from one walk up its layers (R7 §2).
//!
//! Each layer's cured pixels are cut into parts ([`crate::parts`]); a part that stands on
//! nothing of the layer below is an island and gets its island's pattern ([`crate::island`]).
//! Every other part carries the supports of the parts it stands on (joined where parts join,
//! copied where one splits), less those more than [`NEARBY`] outside it, and checks what it adds
//! over the layer below:
//! - a shelf, some of it more than [`SHELF`] out from the layer below: what lies beyond
//!   [`LAND`] gets a pattern as a thick island's, its row on the shelf's own outline;
//! - its new outline (the coast): samples [`COAST`] apart, and a point at each no support
//!   reaches. A support reaches farther as more of the part stands above it ([`reach`]).
//!
//! The points lie on their layers' planes; each then moves onto the part's surface, straight up
//! or down where that is within a layer, else to the nearest place on it, and takes the
//! surface's normal there.

use crate::grid::PixelGrid;
use crate::image::LayerStack;
use crate::island::{island_points, shelf_points, Mask, Pattern, Placed, CELLS};
use crate::parts::{cured, links, over, LayerParts, Part, Run};
use crate::section::{Near, Section};
use makepad_fab_slicer::par;
use std::sync::Arc;

/// Supports farther than this outside a part don't hold it (mm).
pub const NEARBY: f64 = 5.0;
/// A part's new area this far out from the layer below is a shelf (mm)...
pub const SHELF: f64 = 2.0;
/// ...and beyond this, it needs points of its own (mm).
pub const LAND: f64 = 1.5;
/// The coast's samples' spacing (mm).
pub const COAST: f64 = 2.0;

/// How the points are placed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointSettings {
    /// The tip's radius (mm).
    pub tip_radius: f64,
    /// 1: as R7's patterns; more is denser.
    pub density: f64,
}

impl Default for PointSettings {
    fn default() -> Self {
        PointSettings { tip_radius: 0.25, density: 1.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PointKind {
    /// Holds an island.
    Island,
    /// Holds a shelf beyond the layer below.
    Shelf,
    /// Holds a new stretch of outline no support reached.
    Coast,
}

/// Where a support touches the part (printer mm), the part's surface normal there (outward), and
/// the layer it was placed on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SupportPoint {
    pub at: [f64; 3],
    pub normal: [f64; 3],
    pub kind: PointKind,
    pub layer: u32,
}

/// How far a support reaches along a layer `above` mm over it (mm): R7's curve of radius on the
/// height of part built above it, `density` times as dense across.
pub fn reach(above: f64, density: f64) -> f64 {
    const CURVE: [(f64, f64); 4] = [(0.0, 3.2), (3.9, 4.0), (15.0, 5.0), (40.0, 6.0)];
    let h = above.max(0.0);
    let k = CURVE.partition_point(|(z, _)| *z <= h);
    let r = if k >= CURVE.len() {
        CURVE[CURVE.len() - 1].1
    } else {
        let ((z0, r0), (z1, r1)) = (CURVE[k - 1], CURVE[k]);
        r0 + (r1 - r0) * (h - z0) / (z1 - z0)
    };
    r / density.max(0.1).sqrt()
}

/// What a part of a layer adds, worked out before the walk up the layers: an island's pattern,
/// or the shelf points and the coast samples of what it has over the layer below.
enum Adds {
    Island(Vec<Placed>),
    Over { shelf: Vec<[f64; 2]>, coast: Vec<[f64; 2]> },
    Nothing,
}

/// A support while the walk carries it: where on the bed, and the height it was placed at.
#[derive(Clone, Copy, Debug)]
struct Held {
    xy: [f64; 2],
    z: f64,
}

/// The support points of `stack`'s parts.
pub fn support_points(stack: &LayerStack, s: &PointSettings) -> Vec<SupportPoint> {
    points_of(stack, &cured(stack), s)
}

/// The support points of `stack`'s parts from its layers' cured parts (`layers`, [`cured`]).
pub fn points_of(stack: &LayerStack, layers: &[LayerParts], s: &PointSettings) -> Vec<SupportPoint> {
    let grid = stack.grid;
    let pattern = Pattern::new(s.tip_radius, s.density);
    let n = stack.layers.count as usize;
    assert_eq!(layers.len(), n, "a cured layer for each layer");
    let on: Vec<Vec<Vec<u32>>> = par::map(n, |k| if k == 0 { vec![Vec::new(); layers[0].parts.len()] } else { links(&layers[k - 1], &layers[k]) });
    // the islands' patterns and what each other part adds over the layer below, all at once:
    // each holds only its own shape and the layers' (the parts on the plate need none), the
    // largest islands first so the longest start first
    let first: Vec<usize> = layers.iter().scan(0, |at, l| Some(std::mem::replace(at, *at + l.parts.len()))).collect();
    let mut jobs: Vec<(usize, usize)> = (1..n).flat_map(|k| (0..layers[k].parts.len()).map(move |p| (k, p))).collect();
    jobs.sort_by_key(|&(k, p)| (!on[k][p].is_empty(), std::cmp::Reverse(if on[k][p].is_empty() { layers[k].parts[p].pixels } else { 0 })));
    let done: Vec<Adds> = par::map(jobs.len(), |i| {
        let (k, p) = jobs[i];
        let part = &layers[k].parts[p];
        if on[k][p].is_empty() {
            return Adds::Island(island_points(&Mask::of_runs(&part.runs, part.rect, &grid, CELLS), &pattern));
        }
        let new = over(part, &layers[k - 1]);
        if new.is_empty() {
            return Adds::Nothing;
        }
        Adds::Over { shelf: shelf(part, &new, &layers[k - 1], &grid, &pattern), coast: coast(&new, &layers[k], &grid) }
    });
    let mut adds_of: Vec<Adds> = (0..first.last().map_or(0, |f| f + layers[n - 1].parts.len())).map(|_| Adds::Nothing).collect();
    for ((k, p), a) in jobs.into_iter().zip(done) {
        adds_of[first[k] + p] = a;
    }
    let mut held: Vec<Held> = Vec::new();
    let mut placed: Vec<([f64; 2], PointKind, u32)> = Vec::new();
    let mut sets: Vec<Vec<u32>> = vec![Vec::new(); layers.first().map_or(0, |l| l.parts.len())];
    for k in 1..n {
        let z = stack.layers.z(k as u32);
        let mut next = Vec::with_capacity(layers[k].parts.len());
        for (pi, part) in layers[k].parts.iter().enumerate() {
            let mut set: Vec<u32> = Vec::new();
            let put = |held: &mut Vec<Held>, placed: &mut Vec<([f64; 2], PointKind, u32)>, set: &mut Vec<u32>, xy: [f64; 2], kind: PointKind| {
                set.push(held.len() as u32);
                held.push(Held { xy, z });
                placed.push((xy, kind, k as u32));
            };
            let adds = std::mem::replace(&mut adds_of[first[k] + pi], Adds::Nothing);
            if let Adds::Island(pattern) = adds {
                for q in pattern {
                    put(&mut held, &mut placed, &mut set, q.at, PointKind::Island);
                }
                next.push(set);
                continue;
            }
            // the supports of the parts it stands on, those near it
            for b in &on[k][pi] {
                set.extend_from_slice(&sets[*b as usize]);
            }
            set.sort_unstable();
            set.dedup();
            set.retain(|h| near_part(part, &grid, held[*h as usize].xy, NEARBY));
            let Adds::Over { shelf, coast } = adds else {
                next.push(set);
                continue;
            };
            // a shelf: its far part held as a thick island's
            for at in shelf {
                put(&mut held, &mut placed, &mut set, at, PointKind::Shelf);
            }
            // the coast, where no support reaches: each support's reach at this height once
            let mut reaches: Vec<([f64; 2], f64)> = set.iter().map(|h| (held[*h as usize].xy, reach(z - held[*h as usize].z, s.density))).collect();
            for at in coast {
                // a support more than its reach away along either axis is farther than that
                let reached = reaches.iter().any(|&(xy, r)| (xy[0] - at[0]).abs() <= r && (xy[1] - at[1]).abs() <= r && dist(xy, at) <= r);
                if !reached {
                    put(&mut held, &mut placed, &mut set, at, PointKind::Coast);
                    reaches.push((at, reach(z - z, s.density)));
                }
            }
            next.push(set);
        }
        sets = next;
    }
    snap(stack, &placed)
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Whether bed point `p` is within `d` of one of `part`'s pixels.
fn near_part(part: &Part, grid: &PixelGrid, p: [f64; 2], d: f64) -> bool {
    let [px, py] = grid.pitch;
    let r = &part.rect;
    let (lo, hi) = (grid.at(r.x0 as f64, r.y0 as f64), grid.at(r.x1 as f64, r.y1 as f64));
    if p[0] < lo[0] - d || p[0] > hi[0] + d || p[1] < lo[1] - d || p[1] > hi[1] + d {
        return false;
    }
    let (y0, y1) = (((p[1] - d) / py).floor(), ((p[1] + d) / py).ceil());
    let first = part.runs.partition_point(|run| (run.y as f64) < y0);
    for run in &part.runs[first..] {
        if run.y as f64 > y1 {
            break;
        }
        // the gap from p to the run's box of pixels
        let gx = (run.x0 as f64 * px - p[0]).max(p[0] - run.x1 as f64 * px).max(0.0);
        let gy = (run.y as f64 * py - p[1]).max(p[1] - (run.y + 1) as f64 * py).max(0.0);
        // more than `d` along either axis is farther than that
        if gx <= d && gx.hypot(gy) <= d {
            return true;
        }
    }
    false
}

/// The cells (a quarter millimetre) a shelf's distance from the layer below is measured on.
const SHELF_CELL: f64 = 0.25;

/// Points for a shelf of `part`: where its new area (`new`) lies more than [`SHELF`] out from
/// what `below` cures, what lies beyond [`LAND`] gets a thick island's pattern.
fn shelf(part: &Part, new: &[Run], below: &LayerParts, grid: &PixelGrid, pattern: &Pattern) -> Vec<[f64; 2]> {
    // most new area lies along the layer below: when all of it is within reach along its row or
    // its column, nothing is far by the cells either (they read a cell's distance up to half its
    // diagonal short)
    let slack = 0.5 * (SHELF_CELL / grid.pitch[0]).round().max(1.0) * grid.pitch[0].hypot(grid.pitch[1]);
    if beside(new, below, grid, SHELF - slack) {
        return Vec::new();
    }
    // the new area's box, grown by the shelf's reach, in coarse cells; the cells below cures any
    // pixel of are where the distance is measured from
    let mut r = crate::grid::PixelRect::EMPTY;
    for run in new {
        r = r.union(&crate::grid::PixelRect { x0: run.x0, y0: run.y, x1: run.x1, y1: run.y + 1 });
    }
    let grow = [(SHELF / grid.pitch[0]).ceil() as u32 + 1, (SHELF / grid.pitch[1]).ceil() as u32 + 1];
    let r = crate::grid::PixelRect { x0: r.x0.saturating_sub(grow[0]), y0: r.y0.saturating_sub(grow[1]), x1: r.x1 + grow[0], y1: r.y1 + grow[1] };
    let f = [(SHELF_CELL / grid.pitch[0]).round().max(1.0) as u32, (SHELF_CELL / grid.pitch[1]).round().max(1.0) as u32];
    let (w, h) = (r.width().div_ceil(f[0]) as usize, r.height().div_ceil(f[1]) as usize);
    let mut land = vec![false; w * h];
    let mut any = false;
    for y in r.y0..r.y1 {
        for b in below.row(y) {
            let (a, e) = (b.x0.max(r.x0), b.x1.min(r.x1));
            let mut x = a;
            while x < e {
                let i = ((x - r.x0) / f[0]) as usize;
                land[((y - r.y0) / f[1]) as usize * w + i] = true;
                any = true;
                x = r.x0 + (i as u32 + 1) * f[0];
            }
        }
    }
    if !any {
        return Vec::new();
    }
    let cell = [f[0] as f64 * grid.pitch[0], f[1] as f64 * grid.pitch[1]];
    let d2 = crate::island::distance_squared(w, h, cell, |k| land[k]);
    // a cell's distance is its centre's; a pixel may lie half a cell nearer
    let slack = 0.5 * cell[0].hypot(cell[1]);
    let far = |x: u32, y: u32, d: f64| d2[((y - r.y0) / f[1]) as usize * w + ((x - r.x0) / f[0]) as usize].sqrt() - slack > d;
    if !new.iter().any(|run| (run.x0..run.x1).any(|x| far(x, run.y, SHELF))) {
        return Vec::new();
    }
    // the shelf beyond the land, on the part's own cells (their depth is from its outline)
    let mut beyond: Vec<Run> = Vec::new();
    for run in new {
        let mut x = run.x0;
        while x < run.x1 {
            while x < run.x1 && !far(x, run.y, LAND) {
                x += 1;
            }
            let x0 = x;
            while x < run.x1 && far(x, run.y, LAND) {
                x += 1;
            }
            if x > x0 {
                beyond.push(Run { y: run.y, x0, x1: x });
            }
        }
    }
    let m = Mask::of_runs(&part.runs, part.rect, grid, CELLS);
    let mut region = vec![false; m.on.len()];
    for run in &beyond {
        for x in run.x0..run.x1 {
            let c = grid.center(x, run.y);
            let k = m.cell_at(c);
            region[k] = m.on[k];
        }
    }
    shelf_points(&m, &region, pattern)
}

/// Whether every pixel of `new` lies within `d` (mm, centre to centre) of a pixel `below` cures,
/// along its row or along its column: no farther, then, across the plane.
fn beside(new: &[Run], below: &LayerParts, grid: &PixelGrid, d: f64) -> bool {
    let (dx, dy) = ((d / grid.pitch[0]).floor() as u32, (d / grid.pitch[1]).floor() as u32);
    for run in new {
        // the columns of the run no cured pixel of its row reaches
        let mut x = run.x0;
        for b in below.row(run.y) {
            let (lo, hi) = (b.x0.saturating_sub(dx), b.x1 + dx);
            if hi <= x {
                continue;
            }
            if lo >= run.x1 {
                break;
            }
            for c in x..lo.max(x) {
                if !(run.y.saturating_sub(dy)..=run.y + dy).any(|y| below.cured(c, y)) {
                    return false;
                }
            }
            x = x.max(hi);
            if x >= run.x1 {
                break;
            }
        }
        for c in x..run.x1 {
            if !(run.y.saturating_sub(dy)..=run.y + dy).any(|y| below.cured(c, y)) {
                return false;
            }
        }
    }
    true
}

/// Samples of the coast: the pixels of the new area (`new`) on the part's outline in `layer`
/// (beside a pixel it doesn't cure), each farther than [`COAST`] from the samples before it, in
/// order along the rows.
fn coast(new: &[Run], layer: &LayerParts, grid: &PixelGrid) -> Vec<[f64; 2]> {
    let mut out: Vec<[f64; 2]> = Vec::new();
    for run in new {
        for x in run.x0..run.x1 {
            let y = run.y;
            let edge = !layer.cured(x.wrapping_sub(1), y) || !layer.cured(x + 1, y) || y == 0 || !layer.cured(x, y - 1) || !layer.cured(x, y + 1);
            if !edge {
                continue;
            }
            let c = grid.center(x, y);
            // the samples go row by row, so only those of the last rows can be near
            if out.iter().rev().take_while(|o| (c[1] - o[1]).abs() <= COAST).all(|o| dist(*o, c) > COAST) {
                out.push(c);
            }
        }
    }
    out
}

/// The points onto the surface of the stack's parts: from each point on its layer's plane,
/// straight up or down to the surface if that is within a layer, else to its nearest place;
/// with the outward normal there.
fn snap(stack: &LayerStack, placed: &[([f64; 2], PointKind, u32)]) -> Vec<SupportPoint> {
    let h = stack.layers.height;
    let chunks: Vec<&[([f64; 2], PointKind, u32)]> = placed.chunks(256).collect();
    par::map(chunks.len(), |c| {
        let chunk = chunks[c];
        let start: Vec<[f64; 3]> = chunk.iter().map(|(xy, _, k)| [xy[0], xy[1], stack.layers.z(*k)]).collect();
        // straight up or down: Newton's steps along z, each at most a layer
        let mut p = start.clone();
        for _ in 0..8 {
            let near = nearest(&stack.sections, &p);
            for (q, n) in p.iter_mut().zip(&near) {
                if n.normal[2].abs() > 0.2 && n.distance.abs() > 1e-5 {
                    q[2] -= (n.distance / n.normal[2]).clamp(-h, h);
                }
            }
        }
        let near = nearest(&stack.sections, &p);
        // where that failed (slipped off, or left the layer): to the nearest place on it
        let mut q: Vec<[f64; 3]> = start.clone();
        for _ in 0..8 {
            let n = nearest(&stack.sections, &q);
            for (q, n) in q.iter_mut().zip(&n) {
                for a in 0..3 {
                    q[a] -= n.distance * n.normal[a];
                }
            }
        }
        let near_q = nearest(&stack.sections, &q);
        chunk
            .iter()
            .enumerate()
            .map(|(i, (_, kind, layer))| {
                let vertical = near[i].distance.abs() < 0.01 && (p[i][2] - start[i][2]).abs() <= h;
                let (at, n) = if vertical { (p[i], near[i]) } else { (q[i], near_q[i]) };
                SupportPoint { at, normal: n.normal, kind: *kind, layer: *layer }
            })
            .collect::<Vec<_>>()
    })
    .into_iter()
    .flatten()
    .collect()
}

/// The solid of `sections` at each point: the nearest surface among the parts near it.
pub(crate) fn nearest(sections: &[Arc<dyn Section>], p: &[[f64; 3]]) -> Vec<Near> {
    let mut best = vec![Near { distance: f64::INFINITY, normal: [0.0, 0.0, -1.0] }; p.len()];
    let mut out = Vec::new();
    for s in sections {
        let (lo, hi) = s.bounds();
        let inside = |q: &[f64; 3]| (0..3).all(|a| q[a] >= lo[a] - 1.0 && q[a] <= hi[a] + 1.0);
        let which: Vec<usize> = (0..p.len()).filter(|i| inside(&p[*i])).collect();
        if which.is_empty() {
            continue;
        }
        let at: Vec<[f64; 3]> = which.iter().map(|i| p[*i]).collect();
        out.clear();
        out.resize(at.len(), Near::default());
        s.near(&at, &mut out);
        for (i, n) in which.iter().zip(&out) {
            if n.distance < best[*i].distance {
                best[*i] = *n;
            }
        }
    }
    best
}
