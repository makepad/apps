//! Ready-made support overrides: an `m.support_block` for a piece that won't come out and an
//! `m.support_enforce` for a patch the plan leaves, each a region in the object's own frame (its
//! script's mm, Y up), so it goes with the object wherever it is placed, turned or copied, as paint
//! does. They are offers: nothing here writes a script. The planner, the paint and the region
//! language are the ones the scripts already have.

use crate::supports::Surface;
use fab_model::weave::affine;
use makepad_fab_slicer::geom::{ExPolygon, Pt};
use makepad_fab_toolpath::skeleton::{Skeleton, SkeletonPiece};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// What a region takes round what it covers (plate mm): more than a layer, so the face's own
/// layer plane lies inside it, and less than a contact spacing, so it stays off its neighbours.
const MARGIN: f64 = 0.5;
/// A block is split into at most this many boxes to keep what other pieces stand in out of it.
const MOST_BOXES: usize = 4;

/// A part as overrides are written for it: its frame and script, and the room a support takes.
pub struct Placed {
    /// Plate mm to the object's own frame.
    pub to_model: [[f64; 4]; 3],
    /// Model mm per plate mm.
    pub shrink: f64,
    pub script: Option<String>,
    /// How far under a face a support still holds it (mm).
    pub below: f64,
    /// The least room under a face a support fits in (mm).
    pub least_clear: f64,
}

impl Placed {
    pub fn new(model_to_plate: &[[f64; 4]; 3], script: Option<String>, below: f64, least_clear: f64) -> Placed {
        let m = model_to_plate;
        let stretch = (m[0][0] * m[0][0] + m[1][0] * m[1][0] + m[2][0] * m[2][0]).sqrt().max(1e-9);
        Placed { to_model: fab_model::weave::invert(m), shrink: 1.0 / stretch, script, below, least_clear }
    }
}

/// A part's surface as a piece's block is written for it: what the block's overhang is measured on.
pub struct Side {
    pub placed: Placed,
    pub surface: Arc<Surface>,
    /// Each facet's overhang degree (0: none).
    pub degrees: Arc<Vec<u8>>,
}

impl Side {
    fn corners(&self, t: usize) -> [[f64; 3]; 3] {
        self.surface.mesh.triangles[t].map(|v| self.surface.mesh.positions[v as usize])
    }

    /// The overhang area (mm²) whose facets' middles lie in the boxes (model frame).
    fn area_in(&self, boxes: &[([f64; 3], [f64; 3])]) -> f64 {
        let mut area = 0.0;
        for (t, &d) in self.degrees.iter().enumerate() {
            if d == 0 {
                continue;
            }
            let c = self.corners(t);
            let middle = affine(&self.placed.to_model, std::array::from_fn(|k| (c[0][k] + c[1][k] + c[2][k]) / 3.0));
            if boxes.iter().any(|b| inside(middle, b)) {
                area += facet_area(c);
            }
        }
        area
    }
}

/// An offer: the statement for the script, what it gives up and what else it reaches.
pub struct Override {
    pub script: Option<String>,
    pub statement: String,
    /// Overhang inside the region (mm²): with a block it prints with nothing under it.
    pub area_mm2: f64,
    /// Other pieces standing in or holding inside the region.
    pub others: usize,
}

pub fn facet_area(c: [[f64; 3]; 3]) -> f64 {
    let (u, v) = ([c[1][0] - c[0][0], c[1][1] - c[0][1], c[1][2] - c[0][2]], [c[2][0] - c[0][0], c[2][1] - c[0][1], c[2][2] - c[0][2]]);
    0.5 * ((u[1] * v[2] - u[2] * v[1]).powi(2) + (u[2] * v[0] - u[0] * v[2]).powi(2) + (u[0] * v[1] - u[1] * v[0]).powi(2)).sqrt()
}

fn inside(p: [f64; 3], b: &([f64; 3], [f64; 3])) -> bool {
    (0..3).all(|k| p[k] >= b.0[k] && p[k] <= b.1[k])
}

/// Which chains something merges into: a chain nothing merges into starts at a tip.
pub fn joined(sk: &Skeleton) -> Vec<bool> {
    let mut joined = vec![false; sk.chains.len()];
    for chain in &sk.chains {
        if let Some(into) = chain.into {
            joined[into] = true;
        }
    }
    joined
}

/// An island's outline and what lies inside it, a reach apart (plate mm).
pub fn region_points(e: &ExPolygon, at: [f64; 2], z: f64, reach: f64, out: &mut Vec<[f64; 3]>) {
    for p in e.contour.pts.iter().step_by(4) {
        out.push([p.x_mm() + at[0], p.y_mm() + at[1], z]);
    }
    let b = e.bbox();
    let (x0, y0, x1, y1) = (b.min.x_mm(), b.min.y_mm(), b.max.x_mm(), b.max.y_mm());
    let (nx, ny) = (((x1 - x0) / reach).ceil() as usize, ((y1 - y0) / reach).ceil() as usize);
    for i in 0..=nx {
        for j in 0..=ny {
            let (x, y) = (x0 + i as f64 * reach, y0 + j as f64 * reach);
            if e.contains(Pt::from_mm(x, y)) {
                out.push([x + at[0], y + at[1], z]);
            }
        }
    }
}

/// Where a piece meets the part, a reach apart (plate mm): its tips, its pads and its footprints.
pub fn piece_contacts(sk: &Skeleton, of: &SkeletonPiece, at: [f64; 2], reach: f64, joined: &[bool]) -> Vec<[f64; 3]> {
    let mut out = Vec::new();
    for &k in &of.chains {
        let chain = &sk.chains[k];
        if let (false, Some(&(x, y, _))) = (joined[k], chain.sections.first()) {
            out.push([x + at[0], y + at[1], sk.heights[chain.top].0]);
        }
    }
    for (l, e) in &of.roofs {
        region_points(e, at, sk.heights.get(*l).map_or(0.0, |h| h.0), reach, &mut out);
    }
    for (s, e) in &of.slabs {
        region_points(e, at, sk.slabs[*s].z, reach, &mut out);
    }
    out
}

/// Where a piece stands (plate mm): its contacts and the middle of every section of its branches.
pub fn piece_points(sk: &Skeleton, of: &SkeletonPiece, at: [f64; 2], reach: f64, joined: &[bool]) -> Vec<[f64; 3]> {
    let mut out = piece_contacts(sk, of, at, reach, joined);
    for &k in &of.chains {
        let chain = &sk.chains[k];
        for (j, &(x, y, _)) in chain.sections.iter().enumerate() {
            if let Some(h) = chain.top.checked_sub(j).and_then(|l| sk.heights.get(l)) {
                out.push([x + at[0], y + at[1], h.0 - h.1 / 2.0]);
            }
        }
    }
    out
}

/// Points in columns a reach wide: is one of them within reach of here, `below` under it or a
/// reach over it (a slope's tip stands higher up the slope).
pub struct Reach {
    reach: f64,
    cells: HashMap<(i64, i64), Vec<[f32; 3]>>,
}

impl Reach {
    pub fn new(reach: f64) -> Reach {
        Reach { reach, cells: HashMap::new() }
    }

    pub fn add(&mut self, p: [f64; 3]) {
        self.cells.entry(((p[0] / self.reach).floor() as i64, (p[1] / self.reach).floor() as i64)).or_default().push(p.map(|v| v as f32));
    }

    pub fn reaches(&self, p: [f64; 3], below: f64) -> bool {
        let (cx, cy) = ((p[0] / self.reach).floor() as i64, (p[1] / self.reach).floor() as i64);
        let r2 = (self.reach * self.reach) as f32;
        for i in cx - 1..=cx + 1 {
            for j in cy - 1..=cy + 1 {
                let Some(points) = self.cells.get(&(i, j)) else { continue };
                for q in points {
                    let (dx, dy) = (q[0] - p[0] as f32, q[1] - p[1] as f32);
                    let dz = q[2] as f64 - p[2];
                    if dx * dx + dy * dy <= r2 && dz >= -below && dz <= self.reach {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// Up to 64 x 64 samples of a facet a spacing apart, each with its share of the facet.
pub fn facet_samples(c: [[f64; 3]; 3], spacing: f64, mut each: impl FnMut([f64; 3], f64)) {
    let d = |a: [f64; 3], b: [f64; 3]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
    let longest = d(c[0], c[1]).max(d(c[1], c[2])).max(d(c[2], c[0]));
    let n = ((longest / spacing).ceil() as usize).clamp(1, 64);
    let share = 1.0 / (n * n) as f64;
    for i in 0..n {
        for j in 0..n - i {
            // Two small triangles' worth at each stop but the edge's: their centroids.
            for flip in 0..2 {
                if flip == 1 && i + j + 1 >= n {
                    continue;
                }
                let (u, v) = if flip == 0 { ((i as f64 + 1.0 / 3.0) / n as f64, (j as f64 + 1.0 / 3.0) / n as f64) } else { ((i as f64 + 2.0 / 3.0) / n as f64, (j as f64 + 2.0 / 3.0) / n as f64) };
                each(std::array::from_fn(|k| c[0][k] + u * (c[1][k] - c[0][k]) + v * (c[2][k] - c[0][k])), share);
            }
        }
    }
}

/// The corners of the overhang facets a piece's contacts hold (plate mm): within reach in plan,
/// from a reach under a contact to `below` over it. A piece holding none (a pad under a bridge's
/// edge): its contacts.
pub fn held_points(side: &Side, contacts: &[[f64; 3]], reach: f64) -> Vec<[f64; 3]> {
    let mut held = Reach::new(reach);
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for &p in contacts {
        held.add(p);
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let (lo, hi) = ([lo[0] - reach, lo[1] - reach, lo[2] - reach], [hi[0] + reach, hi[1] + reach, hi[2] + side.placed.below]);
    let mut points = Vec::new();
    for (t, &d) in side.degrees.iter().enumerate() {
        if d == 0 {
            continue;
        }
        let c = side.corners(t);
        if (0..3).any(|k| c.iter().all(|p| p[k] < lo[k]) || c.iter().all(|p| p[k] > hi[k])) {
            continue;
        }
        let mut holds = false;
        facet_samples(c, reach, |p, _| holds = holds || held.reaches(p, side.placed.below));
        if holds {
            points.extend(c);
        }
    }
    if points.is_empty() {
        points.extend_from_slice(contacts);
    }
    points
}

/// The block for a piece: what it holds ([`held_points`]), boxed in the object's frame with a
/// margin, split where other pieces' points (`others`, plate mm) would fall inside.
pub fn block_for(side: &Side, held: &[[f64; 3]], others: &[[f64; 3]]) -> Option<Override> {
    if held.is_empty() {
        return None;
    }
    let placed = &side.placed;
    let points: Vec<[f64; 3]> = held.iter().map(|&p| affine(&placed.to_model, p)).collect();
    let others: Vec<[f64; 3]> = others.iter().map(|&p| affine(&placed.to_model, p)).collect();
    let boxes = split_boxes(points, &others, MARGIN * placed.shrink);
    let taken = others.iter().filter(|p| boxes.iter().any(|b| inside(**p, b))).count();
    Some(Override { script: placed.script.clone(), statement: format!("m.support_block({})", region_text(&boxes)), area_mm2: side.area_in(&boxes), others: taken })
}

/// The enforce for an overhang of `area_mm2`: its outlines' points (plate mm) boxed in the
/// object's frame with a margin.
pub fn enforce_around(placed: &Placed, points: &[[f64; 3]], area_mm2: f64) -> Override {
    let points: Vec<[f64; 3]> = points.iter().map(|&p| affine(&placed.to_model, p)).collect();
    let boxes = vec![bounds(&points, MARGIN * placed.shrink)];
    Override { script: placed.script.clone(), statement: format!("m.support_enforce({})", region_text(&boxes)), area_mm2, others: 0 }
}

fn bounds(points: &[[f64; 3]], margin: f64) -> ([f64; 3], [f64; 3]) {
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for p in points {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    (lo.map(|v| v - margin), hi.map(|v| v + margin))
}

/// Boxes round `points`: one, split at the median of its longest side while another's point lies
/// in one, up to [`MOST_BOXES`].
fn split_boxes(points: Vec<[f64; 3]>, others: &[[f64; 3]], margin: f64) -> Vec<([f64; 3], [f64; 3])> {
    let mut groups = vec![points];
    loop {
        let boxes: Vec<_> = groups.iter().map(|g| bounds(g, margin)).collect();
        let crowded = (0..groups.len()).filter(|&i| groups[i].len() > 3 && others.iter().any(|p| inside(*p, &boxes[i]))).max_by_key(|&i| groups[i].len());
        let (Some(i), true) = (crowded, groups.len() < MOST_BOXES) else { return boxes };
        let (lo, hi) = boxes[i];
        let axis = (0..3).max_by(|&a, &b| (hi[a] - lo[a]).total_cmp(&(hi[b] - lo[b]))).unwrap_or(0);
        let mut g = groups.swap_remove(i);
        g.sort_by(|a, b| a[axis].total_cmp(&b[axis]));
        let rest = g.split_off(g.len() / 2);
        groups.push(g);
        groups.push(rest);
    }
}

/// A length as a script writes it.
fn length(v: f64) -> String {
    let r = (v * 100.0).round() / 100.0;
    if r == 0.0 { "0".into() } else { format!("{r}mm") }
}

/// Boxes as a region: one `m.box`, or a list of them (their union).
pub fn region_text(boxes: &[([f64; 3], [f64; 3])]) -> String {
    let one = |(lo, hi): &([f64; 3], [f64; 3])| {
        let size: [f64; 3] = std::array::from_fn(|k| hi[k] - lo[k]);
        let at: [f64; 3] = std::array::from_fn(|k| (hi[k] + lo[k]) / 2.0);
        format!("m.box({{size: [{}, {}, {}] at: [{}, {}, {}]}})", length(size[0]), length(size[1]), length(size[2]), length(at[0]), length(at[1]), length(at[2]))
    };
    match boxes {
        [b] => one(b),
        many => format!("[{}]", many.iter().map(one).collect::<Vec<_>>().join(", ")),
    }
}

/// What a piece is made of, as a key: a piece of another plan with the same key is the same piece
/// on the same part, and so has the same way out.
pub fn piece_key(part: u64, at: [f64; 2], sk: &Skeleton, of: &SkeletonPiece) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    part.hash(&mut h);
    at.map(f64::to_bits).hash(&mut h);
    let mut chains: Vec<u64> = of.chains.iter().map(|&k| {
        let c = &sk.chains[k];
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for (j, s) in c.sections.iter().enumerate() {
            let layer = c.top.checked_sub(j).and_then(|l| sk.heights.get(l)).copied().unwrap_or_default();
            [s.0, s.1, s.2, layer.0, layer.1].map(f64::to_bits).hash(&mut h);
        }
        h.finish()
    }).collect();
    chains.sort_unstable();
    chains.hash(&mut h);
    let island = |e: &ExPolygon, h: &mut std::collections::hash_map::DefaultHasher| {
        e.contour.pts.hash(h);
        for hole in &e.holes {
            hole.pts.hash(h);
        }
    };
    for (l, e) in &of.roofs {
        sk.heights.get(*l).map(|h| [h.0, h.1].map(f64::to_bits)).hash(&mut h);
        island(e, &mut h);
    }
    for (s, e) in &of.slabs {
        [sk.slabs[*s].z, sk.slabs[*s].height].map(f64::to_bits).hash(&mut h);
        island(e, &mut h);
    }
    h.finish()
}
