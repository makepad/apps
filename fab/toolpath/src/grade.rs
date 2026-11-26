//! Depth from the part's surfaces, normalised per body or per persistent lobe.

use crate::adaptive::{edt_1d, raster};
use crate::warp;
use makepad_fab_slicer::geom::{bbox_of, scaled, unscaled, BBox, ExPolygon, Pt};
use std::collections::hash_map::DefaultHasher;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

/// Depth voxel width in mm for a part's bounding box.
pub fn cell_for(span: [f64; 3]) -> f64 {
    (span[0] * span[1] * span[2] / 4_000_000.0).cbrt().max(0.5)
}

/// A six-connected solid component, numbered by its first voxel in x-fastest order.
#[derive(Clone, Copy, Debug)]
pub struct Body {
    pub id: u32,
    pub dmax: f64,
    pub voxels: usize,
}

/// A lobe of a body: the voxels whose depth rises to one maximum.
#[derive(Clone, Copy, Debug)]
pub struct Lobe {
    pub body: u32,
    pub peak: f64,
    pub voxels: usize,
}

/// A lobe's maximum stands at least a quarter of its depth (and two cells) above the neck to a deeper one.
const LOBE_RISE: f64 = 0.25;

/// Voxel-centred distances to all surfaces, including the walls of closed cavities.
#[derive(Debug)]
pub struct Depth {
    origin: [f64; 3],
    size: [usize; 3],
    cell: f64,
    inv_cell: f64,
    dist: Vec<f32>,
    ids: Vec<u32>,
    bodies: Vec<Body>,
    // Indexed by body id. Outside and ungraded bodies have scale zero.
    scale: Vec<f64>,
    lobes: Vec<Lobe>,
    // Positive only in multi-lobe bodies. Empty if there are none.
    lobe_norm: Vec<f32>,
}

impl Depth {
    /// Each outline covers (lo, hi] in mm. The intervals are ordered by height.
    pub fn new(layers: &[(f64, f64, &[ExPolygon])], min_depth: f64) -> Depth {
        let mut bb = BBox::empty();
        for &(_, _, outline) in layers {
            for e in outline { bb.merge(&e.bbox()); }
        }
        if bb.is_empty() {
            return Depth { origin: [0.0; 3], size: [0; 3], cell: 0.5, inv_cell: 2.0,
                dist: Vec::new(), ids: Vec::new(), bodies: Vec::new(), scale: vec![0.0], lobes: Vec::new(), lobe_norm: Vec::new() };
        }
        let bottom = layers[0].0;
        let span = [unscaled(bb.max.x - bb.min.x), unscaled(bb.max.y - bb.min.y), layers[layers.len() - 1].1 - bottom];
        let cell = cell_for(span);
        let size = span.map(|mm| (mm / cell).ceil() as usize + 2);
        let origin = [bb.min.x_mm() - cell, bb.min.y_mm() - cell, bottom - cell];
        let mut dist = vec![0.0; size[0] * size[1] * size[2]];
        seed_layers(&mut dist, size, cell, bb.min, layers);
        surface_distance(&mut dist, size, cell as f32);
        let (ids, bodies) = label_bodies(&dist, size);
        let mut scale = Vec::with_capacity(bodies.len() + 1);
        scale.push(0.0);
        scale.extend(bodies.iter().map(|b| if b.dmax < min_depth { 0.0 } else { 1.0 / b.dmax }));
        Depth { origin, size, cell, inv_cell: 1.0 / cell, dist, ids, bodies, scale, lobes: Vec::new(), lobe_norm: Vec::new() }
    }

    pub fn cell(&self) -> f64 { self.cell }

    /// Lower corner of voxel (0, 0, 0) in mm and voxel counts in x, y, z.
    pub fn grid(&self) -> ([f64; 3], [usize; 3]) { (self.origin, self.size) }

    pub fn bodies(&self) -> &[Body] { &self.bodies }

    /// Grade each persistent maximum to its own peak, smoothing over a radius in mm.
    pub fn with_lobes(mut self, smooth: f64) -> Depth {
        let (labels, lobes) = label_lobes(&self.dist, &self.ids, self.size, self.cell);
        self.lobes = lobes;
        let mut counts = vec![0usize; self.bodies.len() + 1];
        for lobe in &self.lobes { counts[lobe.body as usize] += 1; }
        if counts.iter().any(|&n| n >= 2) {
            self.lobe_norm = labels.into_iter().map(|id| {
                if id == 0 { return 0.0; }
                let lobe = &self.lobes[id as usize - 1];
                if counts[lobe.body as usize] >= 2 { lobe.peak as f32 } else { 0.0 }
            }).collect();
            let radius = ((smooth / self.cell).round() as usize).max(1);
            smooth_lobe_norm(&mut self.lobe_norm, self.size, radius);
        }
        self
    }

    pub fn lobes(&self) -> &[Lobe] { &self.lobes }

    // Integer coordinates here are voxel centres. Beyond the outermost centres
    // the padding gives both distance and id zero, including on an empty grid.
    #[inline]
    fn coordinates(&self, p: [f64; 3]) -> Option<[f64; 3]> {
        let q = std::array::from_fn(|a| (p[a] - self.origin[a]) * self.inv_cell - 0.5);
        (0..3).all(|a| q[a] >= 0.0 && q[a] < self.size[a] as f64 - 1.0).then_some(q)
    }

    #[inline]
    fn interpolate(&self, q: [f64; 3]) -> f32 {
        let [x, y, z] = q.map(|v| v as usize);
        let [nx, ny, _] = self.size;
        let i = (z * ny + y) * nx + x;
        let (fx, fy, fz) = ((q[0] - x as f64) as f32, (q[1] - y as f64) as f32, (q[2] - z as f64) as f32);
        let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let row = |i: usize| lerp(self.dist[i], self.dist[i + 1], fx);
        let plane = |i: usize| lerp(row(i), row(i + nx), fy);
        lerp(plane(i), plane(i + nx * ny), fz)
    }

    /// Trilinear distance in mm. Voxel centres outside the solid have distance zero.
    #[inline]
    pub fn depth(&self, p: [f64; 3]) -> f32 {
        self.coordinates(p).map_or(0.0, |q| self.interpolate(q))
    }

    #[inline]
    fn voxel(&self, q: [f64; 3]) -> usize {
        let [x, y, z] = q.map(|v| (v + 0.5) as usize);
        (z * self.size[1] + y) * self.size[0] + x
    }

    /// Normalising depth in mm, zero outside and in bodies too shallow to grade.
    #[inline]
    pub fn norm(&self, p: [f64; 3]) -> f64 {
        let Some(q) = self.coordinates(p) else { return 0.0 };
        let i = self.voxel(q);
        let id = self.ids[i] as usize;
        if self.scale[id] == 0.0 { return 0.0; }
        let peak = self.lobe_norm.get(i).copied().unwrap_or(0.0);
        if peak > 0.0 { peak as f64 } else { self.bodies[id - 1].dmax }
    }

    /// Normalised depth and the id of the voxel containing p (zero outside).
    #[inline]
    pub fn at(&self, p: [f64; 3]) -> (f64, u32) {
        let Some(q) = self.coordinates(p) else { return (0.0, 0) };
        let i = self.voxel(q);
        let id = self.ids[i];
        let scale = self.scale[id as usize];
        if scale == 0.0 { return (0.0, id); }
        let d = self.interpolate(q) as f64;
        let peak = self.lobe_norm.get(i).copied().unwrap_or(0.0);
        let t = if peak > 0.0 { d / peak as f64 } else { d * scale };
        (t.clamp(0.0, 1.0), id)
    }
}

// Only write the interior of the padded grid. A gap, a hole, or an empty
// outline stays a zero seed.
fn seed_layers(dist: &mut [f32], [nx, ny, nz]: [usize; 3], cell: f64, min: Pt, layers: &[(f64, f64, &[ExPolygon])]) {
    let (width, height) = (nx - 2, ny - 2);
    if width == 0 || height == 0 { return; }
    let mut mask = vec![false; width * height];
    let raster_cell = scaled(cell); // Fixed-point XY coordinates, in nanometres.
    let bottom = layers[0].0;
    let centre = |z: usize| bottom + (z as f64 - 0.5) * cell;
    let plane = nx * ny;
    let mut z = 1;
    for &(lo, hi, outline) in layers {
        while z < nz - 1 && centre(z) <= lo { z += 1; }
        let start = z;
        while z < nz - 1 && centre(z) <= hi { z += 1; }
        if start == z || outline.is_empty() { continue; }
        mask.fill(false);
        raster(outline, min.x, min.y, raster_cell, width, height, &mut mask);
        for slab in dist[start * plane..z * plane].chunks_exact_mut(plane) {
            for (row, mask_row) in slab[nx..].chunks_exact_mut(nx).zip(mask.chunks_exact(width)) {
                for (d, &inside) in row[1..nx - 1].iter_mut().zip(mask_row) {
                    if inside { *d = f32::INFINITY; }
                }
            }
        }
    }
}

// Exact squared EDT along x, y, z. Convert to wall distance on the last scatter.
fn surface_distance(dist: &mut [f32], [nx, ny, nz]: [usize; 3], cell: f32) {
    let (mut v, mut zz, mut tmp) = (Vec::new(), Vec::new(), Vec::new());
    let mut line = Vec::with_capacity(ny.max(nz));
    let plane = nx * ny;
    for slab in dist.chunks_exact_mut(plane) {
        for row in slab.chunks_exact_mut(nx) { edt_1d(row, cell, &mut v, &mut zz, &mut tmp); }
        for x in 0..nx {
            line.clear();
            line.extend(slab.chunks_exact(nx).map(|row| row[x]));
            edt_1d(&mut line, cell, &mut v, &mut zz, &mut tmp);
            for (row, &d) in slab.chunks_exact_mut(nx).zip(&line) { row[x] = d; }
        }
    }
    for xy in 0..plane {
        line.clear();
        line.extend(dist.chunks_exact(plane).map(|slab| slab[xy]));
        edt_1d(&mut line, cell, &mut v, &mut zz, &mut tmp);
        for (slab, &d) in dist.chunks_exact_mut(plane).zip(&line) {
            slab[xy] = (d.sqrt() - 0.5 * cell).max(0.0);
        }
    }
}

fn label_bodies(dist: &[f32], [nx, ny, _]: [usize; 3]) -> (Vec<u32>, Vec<Body>) {
    let mut ids = vec![0; dist.len()];
    let mut bodies = Vec::new();
    let mut queue = VecDeque::new();
    let plane = nx * ny;
    for start in 0..dist.len() {
        if dist[start] == 0.0 || ids[start] != 0 { continue; }
        let id = bodies.len() as u32 + 1;
        let mut body = Body { id, dmax: 0.0, voxels: 0 };
        ids[start] = id;
        queue.push_back(start);
        while let Some(i) = queue.pop_front() {
            body.voxels += 1;
            body.dmax = body.dmax.max(dist[i] as f64);
            // Only inside voxels enter the queue, so all six neighbours exist.
            for n in [i - 1, i + 1, i - nx, i + nx, i - plane, i + plane] {
                if dist[n] > 0.0 && ids[n] == 0 {
                    ids[n] = id;
                    queue.push_back(n);
                }
            }
        }
        bodies.push(body);
    }
    (ids, bodies)
}

fn lobe_root(parent: &mut [u32], mut r: u32) -> u32 {
    while parent[r as usize] != r {
        let next = parent[parent[r as usize] as usize];
        parent[r as usize] = next;
        r = next;
    }
    r
}

fn label_lobes(dist: &[f32], ids: &[u32], [nx, ny, _]: [usize; 3], cell: f64) -> (Vec<u32>, Vec<Lobe>) {
    let mut order: Vec<_> = (0..dist.len()).filter(|&i| dist[i] > 0.0).collect();
    // Positive float bits sort by depth. The stable sort visits equal depths in voxel order.
    order.sort_by_key(|&i| std::cmp::Reverse(dist[i].to_bits()));
    let plane = nx * ny;
    let mut labels = vec![0; dist.len()];
    let mut parent = vec![0];
    let mut peak = vec![0.0];
    let mut adjacent = [0; 26];
    for i in order {
        let d = dist[i] as f64;
        let mut deepest = i;
        let mut count = 0;
        let mut last_label = 0;
        // Padding surrounds every inside voxel. The centre still has label zero.
        for slab in [i - plane, i, i + plane] { for row in [slab - nx, slab, slab + nx] {
            for n in row - 1..=row + 1 {
                let label = labels[n];
                if label == 0 || ids[n] != ids[i] { continue; }
                if dist[n] > dist[deepest] || (dist[n] == dist[deepest] && n < deepest) { deepest = n; }
                // Most voxels see just one label. A repeated root at a neck can
                // harmlessly be linked to the same top more than once.
                if label != last_label {
                    adjacent[count] = label;
                    count += 1;
                    last_label = label;
                }
            }
        }}
        labels[i] = match &mut adjacent[..count] {
            [] => {
                let root = parent.len() as u32;
                parent.push(root);
                peak.push(d);
                root
            }
            [only] => lobe_root(&mut parent, *only),
            neighbours => {
                for r in neighbours.iter_mut() { *r = lobe_root(&mut parent, *r); }
                // Roots were created in descending peak order, lower id first at a tie.
                let top = *neighbours.iter().min().unwrap();
                for &r in neighbours.iter() {
                    let p = peak[r as usize];
                    if r != top && p - d < (2.0 * cell).max(LOBE_RISE * p) { parent[r as usize] = top; }
                }
                lobe_root(&mut parent, labels[deepest])
            }
        };
    }

    // Renumber by first voxel after all bumps have merged.
    let mut number = vec![0; parent.len()];
    let mut lobes = Vec::new();
    for (i, label) in labels.iter_mut().enumerate() {
        if *label == 0 { continue; }
        let root = lobe_root(&mut parent, *label) as usize;
        if number[root] == 0 {
            lobes.push(Lobe { body: ids[i], peak: peak[root], voxels: 0 });
            number[root] = lobes.len() as u32;
        }
        *label = number[root];
        lobes[*label as usize - 1].voxels += 1;
    }
    (labels, lobes)
}

fn smooth_lobe_norm(norm: &mut [f32], size: [usize; 3], radius: usize) {
    // Only multi-lobe bodies contribute and receive. Two such bodies closer than the
    // radius may blend a little. Carry sums and counts through all axes before dividing.
    let mut sums: Vec<_> = norm.iter().map(|&v| (v as f64, (v > 0.0) as usize)).collect();
    let mut line = Vec::new();
    let mut stride = 1;
    for len in size {
        let r = radius.min(len - 1);
        for block in sums.chunks_exact_mut(len * stride) {
            for start in 0..stride {
                line.clear();
                line.extend(block[start..].iter().step_by(stride).copied());
                let (mut sum, mut count) = (0.0, 0);
                for &(v, n) in &line[..r] { sum += v; count += n; }
                for (i, out) in block[start..].iter_mut().step_by(stride).enumerate() {
                    if i > r {
                        let (v, n) = line[i - r - 1];
                        sum -= v; count -= n;
                    }
                    if i + r < len {
                        let (v, n) = line[i + r];
                        sum += v; count += n;
                    }
                    *out = (sum, count);
                }
            }
        }
        stride *= len;
    }
    for (v, (sum, count)) in norm.iter_mut().zip(sums) {
        if *v > 0.0 { *v = (sum / count as f64) as f32; }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Adaptive {
    /// Interior density as a factor (0.1 means 10%).
    pub interior: f64,
    pub gradient: warp::Gradient,
    pub lobes: bool,
}

#[derive(Debug)]
pub struct Graded {
    pub warp: warp::Warp,
    pub stretch: f64,
    pub scales: Vec<f64>,
    pub ms: f64,
    /// Whether any body is thick enough to grade: without one the infill keeps the plain pattern.
    pub graded: bool,
}

impl Graded {
    fn solve(p: &warp::Problem, sparse: f64, start: Instant, graded: bool) -> Graded {
        let mut warp = warp::Warp::solve(p, &warp::Weights::default());
        warp.guard(p, sparse);
        let stretch = warp.cells(p).iter().map(|s| s.sigma1).fold(1.0, f64::max);
        let scales = warp.scales().to_vec();
        for (body, s) in scales.iter().enumerate().filter(|(_, s)| **s < 1.0) {
            eprintln!("adaptive density: grading reduced to {s:.2} of what was asked in body {body}, so its lines neither fold nor crowd");
        }
        Graded { warp, stretch, scales, ms: start.elapsed().as_secs_f64() * 1000.0, graded }
    }
}

/// The newest solves stay alive between plans. A part can hold its warp for longer.
static RECENT: Mutex<VecDeque<(u64, Arc<Graded>)>> = Mutex::new(VecDeque::new());
const RECENT_PARTS: usize = 8;
static HELD: Mutex<Vec<(u64, Weak<Graded>)>> = Mutex::new(Vec::new());
static SOLVES: AtomicUsize = AtomicUsize::new(0);

/// How many times a warp was built, including solves of shallow parts.
pub fn solves() -> usize { SOLVES.load(Ordering::Relaxed) }

/// The warp built for this input, while any plan, part or recent solve holds it.
pub fn held(key: u64) -> Option<Arc<Graded>> {
    HELD.lock().unwrap().iter().find(|(k, _)| *k == key)?.1.upgrade()
}

fn hold(key: u64, g: Arc<Graded>) -> Arc<Graded> {
    let mut held = HELD.lock().unwrap();
    // Two parts may have solved the same input concurrently. Both take the first warp.
    if let Some(existing) = held.iter().find(|(k, _)| *k == key).and_then(|(_, w)| w.upgrade()) {
        return existing;
    }
    let mut recent = RECENT.lock().unwrap();
    recent.push_front((key, g.clone()));
    recent.truncate(RECENT_PARTS);
    held.retain(|(_, w)| w.strong_count() != 0);
    held.push((key, Arc::downgrade(&g)));
    g
}

/// [`graded`], shared by every plan of the same outlines and settings.
pub fn graded_shared(key: Option<u64>, layers: &[(f64, f64, &[ExPolygon])], a: &Adaptive, sparse: f64, period: f64) -> Arc<Graded> {
    let key = key.unwrap_or_else(|| {
        let mut h = DefaultHasher::new();
        (a.interior.to_bits(), a.gradient, a.lobes, sparse.to_bits(), period.to_bits()).hash(&mut h);
        for &(z, height, outline) in layers {
            (z.to_bits(), height.to_bits(), outline.len()).hash(&mut h);
            for ring in outline.iter().flat_map(|e| std::iter::once(&e.contour).chain(&e.holes)) {
                ring.pts.len().hash(&mut h);
                for p in &ring.pts { (p.x, p.y).hash(&mut h); }
            }
        }
        h.finish()
    });
    held(key).unwrap_or_else(|| hold(key, Arc::new(graded(layers, a, sparse, period))))
}

/// Build a part's warp from its ordered (print Z, height, mid-layer outline) slices.
pub fn graded(layers: &[(f64, f64, &[ExPolygon])], a: &Adaptive, sparse: f64, period: f64) -> Graded {
    SOLVES.fetch_add(1, Ordering::Relaxed);
    let start = Instant::now();
    let empty = || Graded::solve(&warp::Problem {
        grid: warp::Grid { origin: [0.0; 3], cell: 1.0, n: [1; 3] },
        solid: &[false], t: &[0.0], delta: &[0.0],
    }, sparse, start, false);
    let mut bb = BBox::empty();
    for &(_, _, outline) in layers { bb.merge(&bbox_of(outline)); }
    if bb.is_empty() { return empty(); }
    let bottom = layers[0].0 - layers[0].1;
    let top = layers[layers.len() - 1].0;
    let span = [unscaled(bb.max.x - bb.min.x), unscaled(bb.max.y - bb.min.y), top - bottom];
    let mean_h = layers.iter().map(|l| l.1).sum::<f64>() / layers.len() as f64;
    let stride = ((cell_for(span) / mean_h).floor() as usize).max(1);
    let mut cuts = layers.iter().step_by(stride).map(|&(z, h, outline)| (z - h / 2.0, outline)).peekable();
    let mut entries = Vec::new();
    let mut lo = bottom;
    while let Some((z, outline)) = cuts.next() {
        let hi = cuts.peek().map_or(top, |&(next, _)| (z + next) * 0.5);
        entries.push((lo, hi, outline));
        lo = hi;
    }
    let mut depth = Depth::new(&entries, period);
    if a.lobes {
        depth = depth.with_lobes(0.5 * period);
    }
    if !depth.lobe_norm.is_empty() {
        let mut by_body: Vec<_> = depth.lobes().iter().collect();
        by_body.sort_unstable_by(|a, b| a.body.cmp(&b.body).then_with(|| b.peak.total_cmp(&a.peak)));
        for lobes in by_body.chunk_by(|a, b| a.body == b.body).filter(|s| s.len() >= 2) {
            let peaks = lobes.iter().map(|l| format!("{:.1}", l.peak)).collect::<Vec<_>>().join(" and ");
            eprintln!("adaptive density: body {} graded as {} lobes, deepest {peaks} mm", lobes[0].body, lobes.len());
        }
    }
    // Every body is solved, a shallow one with no shrink: on its own it keeps v = 0, so the
    // extension of a graded neighbour's field never reaches into it.
    let fade = |norm: f64| {
        let u = (norm / period - 1.0).clamp(0.0, 1.0);
        u * u * (3.0 - 2.0 * u)
    };
    if !depth.bodies().iter().any(|b| fade(b.dmax) > 0.0) { return empty(); }

    // Half a period resolves any profile a body thick enough to grade can hold; v is smooth
    // between. At most about 30 k cells keeps the solve of a large part short.
    let cell = (span[0] * span[1] * span[2] / 30_000.0).cbrt().max(0.5 * period).max(1.0);
    let grid = warp::Grid { origin: [bb.min.x_mm(), bb.min.y_mm(), bottom], cell, n: span.map(|s| (s / cell).ceil() as usize) };
    let count = grid.n.iter().product();
    let mut solid = Vec::with_capacity(count);
    let mut t = Vec::with_capacity(count);
    let mut delta = Vec::with_capacity(count);
    let [nx, ny, nz] = grid.n;
    let centre = |axis: usize, i: usize| grid.origin[axis] + (i as f64 + 0.5) * cell;
    for z in 0..nz { for y in 0..ny { for x in 0..nx {
        let p = [centre(0, x), centre(1, y), centre(2, z)];
        let (value, id) = depth.at(p);
        solid.push(id != 0);
        t.push(value as f32);
        delta.push(if id == 0 { 0.0 } else { warp::shrink(a.gradient, sparse, a.interior, value, fade(depth.norm(p))) });
    }}}
    drop(depth);
    if !solid.iter().any(|&s| s) { return empty(); }
    Graded::solve(&warp::Problem { grid, solid: &solid, t: &t, delta: &delta }, sparse, start, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_fab_slicer::geom::Polygon;
    use std::f64::consts::TAU;

    fn rectangle(x0: f64, y0: f64, x1: f64, y1: f64) -> ExPolygon {
        ExPolygon::new(Polygon::rect(Pt::from_mm(x0, y0), Pt::from_mm(x1, y1)))
    }

    fn with_hole(mut outer: ExPolygon, lo: f64, hi: f64) -> ExPolygon {
        let mut hole = Polygon::rect(Pt::from_mm(lo, lo), Pt::from_mm(hi, hi));
        hole.reverse();
        outer.holes.push(hole);
        outer
    }

    fn extrude(outline: &[ExPolygon], count: usize, min_depth: f64) -> Depth {
        let layers: Vec<_> = (0..count).map(|i| (i as f64 * 0.2, (i + 1) as f64 * 0.2, outline)).collect();
        Depth::new(&layers, min_depth)
    }

    fn sphere(stride: usize) -> Depth {
        // Keep the middle cut of each group of 1 or 3 original 0.2 mm layers.
        let cuts: Vec<_> = (0..150).step_by(stride).map(|i| {
            let z = ((i + stride / 2) as f64 + 0.5) * 0.2;
            let r = (225.0 - (z - 15.0).powi(2)).sqrt();
            let pts = (0..96).map(|j| {
                let (sin, cos) = (TAU * j as f64 / 96.0).sin_cos();
                Pt::from_mm(r * cos, r * sin)
            }).collect();
            ExPolygon::new(Polygon::new(pts))
        }).collect();
        let layers: Vec<_> = cuts.iter().enumerate().map(|(j, cut)| {
            ((j * stride) as f64 * 0.2, ((j + 1) * stride) as f64 * 0.2, std::slice::from_ref(cut))
        }).collect();
        Depth::new(&layers, 0.0)
    }

    fn check_sphere_depth(field: &Depth, tolerance: f64) {
        // Same probes for both layer strides, including both poles and the equator.
        for axis in 0..3 {
            for offset in [-14.5_f64, -7.5, 0.0, 7.5, 14.5] {
                let mut p = [0.0, 0.0, 15.0];
                p[axis] += offset;
                let want = 15.0 - offset.abs();
                let d = field.depth(p) as f64;
                assert!((d - want).abs() <= tolerance, "sphere depth at {p:?}: measured {d}, expected {want} within {tolerance} mm");
            }
        }
    }

    fn check_single_lobe(field: Depth, shape: &str) {
        let (origin, size) = field.grid();
        let mut probes = Vec::new();
        for z in 0..=6 { for y in 0..=6 { for x in 0..=6 {
            let p = std::array::from_fn(|a| origin[a] + size[a] as f64 * field.cell() * [x, y, z][a] as f64 / 6.0);
            probes.push((p, field.at(p)));
        }}}
        let field = field.with_lobes(2.5);
        assert_eq!(field.lobes().len(), 1, "{shape} must have one lobe, found {:?}", field.lobes());
        for (p, (t, id)) in probes {
            let (lt, lid) = field.at(p);
            assert_eq!((lt.to_bits(), lid), (t.to_bits(), id), "{shape} grade and body at {p:?}: lobes ({lt}, {lid}), distance ({t}, {id})");
            let expected = if id == 0 { 0.0 } else { field.bodies()[0].dmax };
            assert_eq!(field.norm(p), expected, "{shape} normalising depth at {p:?} in body {id}");
        }
    }

    #[test]
    fn cube_has_one_lobe() {
        check_single_lobe(extrude(&[rectangle(0.0, 0.0, 30.0, 30.0)], 150, 0.0), "30 mm cube");
    }

    #[test]
    fn sphere_grades_from_core_to_surface() {
        let field = sphere(1);
        let tolerance = 1.5 * field.cell();
        check_sphere_depth(&field, tolerance);
        assert_eq!(field.bodies().len(), 1, "sphere must be one body");
        let dmax = field.bodies()[0].dmax;
        assert!((dmax - 15.0).abs() <= tolerance, "sphere maximum depth {dmax} must approximate its radius");
        let (half, id) = field.at([7.5, 0.0, 15.0]);
        assert_eq!(id, 1, "half-radius point must belong to the sphere");
        assert!((half - 0.5).abs() <= 0.1, "normalised half-radius depth {half} must be near one half");
        let mut last = field.at([0.0, 0.0, 15.0]).0;
        for i in 1..=64 {
            let x = i as f64 * 0.25;
            let t = field.at([x, 0.0, 15.0]).0;
            assert!(t <= last, "sphere grade rose from {last} to {t} going outwards at x={x}");
            last = t;
        }
        for p in [[12.0, 12.0, 15.0], [16.0, 0.0, 15.0], [0.0, 0.0, -1.0], [0.0, 0.0, 31.0]] {
            assert_eq!(field.at(p), (0.0, 0), "outside point {p:?} must have zero grade and id");
            assert_eq!(field.depth(p), 0.0, "depth away from the sphere must vanish at {p:?}");
        }
        check_single_lobe(field, "sphere");
    }

    fn density_factor(warp: &warp::Warp, p: [f64; 3]) -> f64 {
        let step = 0.05; // mm, within a solver cell at each core
        let derivative = |axis: usize| {
            let (mut lo, mut hi) = (p, p);
            lo[axis] -= step;
            hi[axis] += step;
            let (lo, hi) = (warp.at(lo), warp.at(hi));
            [(hi[0] - lo[0]) / (2.0 * step), (hi[1] - lo[1]) / (2.0 * step)]
        };
        let ([xx, yx], [xy, yy]) = (derivative(0), derivative(1));
        let (a, b, c, d) = (1.0 - xx, -xy, -yx, 1.0 - yy);
        // For J = I - G, (sigma1 + sigma2)^2 = ||J||_F^2 + 2 |det J|.
        0.5 * (a * a + b * b + c * c + d * d + 2.0 * (a * d - b * c).abs()).sqrt()
    }

    #[test]
    fn lobe_persistence_at_a_neck() {
        // A depth profile along one padded row. The 1.75 peak rises less than two cells
        // and the 10 peak less than a quarter. The 8 peak rises exactly a quarter.
        let start = 9 * 3 + 9 + 1;
        let mut dist = [0.0; 9 * 3 * 3];
        dist[start..start + 7].copy_from_slice(&[1.75, 1.0, 8.0, 6.0, 10.0, 7.75, 12.0]);
        let ids = dist.map(|d| (d > 0.0) as u32);
        let (labels, lobes) = label_lobes(&dist, &ids, [9, 3, 3], 0.5);
        assert_eq!(&labels[start..start + 7], &[1, 1, 1, 2, 2, 2, 2], "sub-threshold bumps must join the persistent peaks in first-voxel order: {lobes:?}");
        let peaks: Vec<_> = lobes.iter().map(|l| (l.peak, l.voxels)).collect();
        assert_eq!(peaks, [(8.0, 3), (12.0, 4)], "each surviving peak must count its merged bumps, got {peaks:?}");
    }

    #[test]
    fn unequal_dumbbell_grades_each_lobe_to_its_core() {
        let (h, sparse) = (0.2, 0.2);
        let period = 2.0 * crate::patterns::GYROID_PERIOD * (0.407 / sparse);
        let big = rectangle(0.0, 0.0, 40.0, 40.0);
        let small = rectangle(50.0, 8.0, 74.0, 32.0);
        let both = [big.clone(), small.clone()];
        let joined = [big.clone(), rectangle(40.0, 17.0, 50.0, 23.0), small];
        let layers: Vec<_> = (0..200).map(|i| {
            let outline = if (45..75).contains(&i) { &joined[..] } else if i < 120 { &both[..] } else { std::slice::from_ref(&big) };
            ((i + 1) as f64 * h, h, outline)
        }).collect();
        let intervals: Vec<_> = layers.iter().map(|&(z, h, outline)| (z - h, z, outline)).collect();
        let (big, small, neck) = ([20.0, 20.0, 20.0], [62.0, 20.0, 12.0], [45.0, 20.0, 12.0]);
        let field = Depth::new(&intervals, period);
        let (big_d, small_d) = (field.at(big).0, field.at(small).0);
        let neck_d = (field.at(neck).0, field.norm(neck));
        let field = field.with_lobes(0.5 * period);
        let (big_l, small_l) = (field.at(big).0, field.at(small).0);
        let lobes = field.lobes();
        eprintln!("dumbbell {lobes:?}, neck (t, norm mm): distance {neck_d:?}, lobes {:?}", (field.at(neck).0, field.norm(neck)));
        assert_eq!(field.bodies().len(), 1, "the neck must connect both cubes, found {:?}", field.bodies());
        assert_eq!(lobes.len(), 2, "the two cores must survive the neck, found {lobes:?}");
        assert_eq!(lobes.iter().map(|l| l.voxels).sum::<usize>(), field.bodies()[0].voxels, "the two lobes must cover the body: {lobes:?}");
        for (lobe, want) in lobes.iter().zip([20.0, 12.0]) {
            assert!((lobe.peak - want).abs() <= 1.5 * field.cell(), "lobe peak {} must be within 1.5 cells of {want} mm", lobe.peak);
        }
        let blended = (40..=124).map(|i| field.norm([i as f64 * 0.5, 20.0, 12.0]))
            .find(|&n| n > lobes[1].peak && n < lobes[0].peak);
        assert!(blended.is_some(), "the neck must blend peaks {} and {} mm, found {blended:?}", lobes[0].peak, lobes[1].peak);
        assert!(small_l >= 0.95 && (small_d - 12.0 / 20.0).abs() <= 0.1, "small core t must reach its lobe peak: lobes {small_l}, distance {small_d}");
        assert!(big_l >= 0.95 && big_d >= 0.95, "big core t must reach the peak both ways: lobes {big_l}, distance {big_d}");
        drop(field);

        let build = |lobes| graded(&layers, &Adaptive { interior: 0.1, gradient: warp::Gradient::Linear, lobes }, sparse, period);
        let (distance, lobes) = (build(false), build(true));
        let (big_d, small_d) = (density_factor(&distance.warp, big), density_factor(&distance.warp, small));
        let (big_l, small_l) = (density_factor(&lobes.warp, big), density_factor(&lobes.warp, small));
        eprintln!("dumbbell factors (big, small): distance ({big_d:.5}, {small_d:.5}), scales {:?}, {:.2} ms; lobes ({big_l:.5}, {small_l:.5}), scales {:?}, {:.2} ms",
            distance.scales, distance.ms, lobes.scales, lobes.ms);
        assert!(small_d - small_l >= 0.1, "small core density factor must drop by at least 0.1: distance {small_d}, lobes {small_l}");
        assert!((big_d - big_l).abs() <= 0.05, "big core density factors must agree within 0.05: distance {big_d}, lobes {big_l}");
    }

    #[test]
    fn sphere_at_every_third_layer_keeps_its_physical_depth() {
        let field = sphere(3);
        let cut_gap = 3.0 * 0.2;
        check_sphere_depth(&field, 1.5 * field.cell() + 0.5 * cut_gap);
    }

    #[test]
    fn box_is_limited_by_its_short_axis() {
        let field = extrude(&[rectangle(0.0, 0.0, 40.0, 20.0)], 50, 0.0);
        assert_eq!(field.cell(), 0.5, "box must use the minimum voxel width");
        assert_eq!(field.grid(), ([-0.5; 3], [82, 42, 22]), "grid must include one padding voxel on each side");
        assert_eq!(field.bodies().len(), 1, "solid box must be one body");
        let dmax = field.bodies()[0].dmax;
        assert!((dmax - 5.0).abs() <= 1.5 * field.cell(), "box maximum depth {dmax} must approximate half its height");
        // At the lower corner only one of eight samples is inside, at depth 0.25.
        assert_eq!(field.depth([0.0; 3]), 0.25 / 8.0, "corner depth must interpolate across all three axes");
        let outside = [-0.125, 10.0, 5.0];
        assert_eq!(field.depth(outside), 0.0625, "wall interpolation must extend into the adjacent outside voxel");
        assert_eq!(field.at(outside), (0.0, 0), "the containing outside voxel must determine the id and grade");
    }

    #[test]
    fn separate_cubes_have_their_own_depth_and_id() {
        let field = extrude(&[rectangle(30.0, 0.0, 40.0, 10.0), rectangle(0.0, 0.0, 10.0, 10.0)], 50, 0.0);
        assert_eq!(field.bodies().len(), 2, "separate cubes must give two bodies");
        for (i, body) in field.bodies().iter().enumerate() {
            let id = i as u32 + 1;
            assert_eq!(body.id, id, "cube ids must follow voxel order despite reversed outline order");
            assert_eq!(body.voxels, 20 * 20 * 20, "cube {id} must count each inside voxel once");
            assert!((body.dmax - 5.0).abs() <= 1.5 * field.cell(), "cube {id} maximum depth {} must approximate half its side", body.dmax);
            let (t, at_id) = field.at([5.0 + 30.0 * i as f64, 5.0, 5.0]);
            assert_eq!(at_id, id, "cube centre must have its body's id");
            assert!((t - 1.0).abs() < 1e-12, "cube {id} must reach full grade at its centre");
        }
        assert_eq!(field.at([20.0, 5.0, 5.0]), (0.0, 0), "gap between the cubes must be outside");
    }

    #[test]
    fn closed_cavity_seeds_distance() {
        let solid = [rectangle(0.0, 0.0, 30.0, 30.0)];
        let hollow = [with_hole(solid[0].clone(), 10.0, 20.0)];
        let layers: Vec<_> = (0..150).map(|i| {
            let outline = if (50..100).contains(&i) { &hollow[..] } else { &solid[..] };
            (i as f64 * 0.2, (i + 1) as f64 * 0.2, outline)
        }).collect();
        let field = Depth::new(&layers, 0.0);
        assert_eq!(field.bodies().len(), 1, "solid around a closed cavity must stay connected");
        assert_eq!(field.at([15.0; 3]), (0.0, 0), "centre of the closed cavity must be outside");
        for p in [[9.0, 15.0, 15.0], [15.0, 15.0, 9.0], [15.0, 15.0, 21.0]] {
            let d = field.depth(p) as f64;
            assert!((d - 1.0).abs() <= 1.5 * field.cell(), "cavity wall must set the 1 mm depth at {p:?}, got {d}");
        }
    }

    #[test]
    fn shallow_plate_keeps_its_body_without_grading() {
        let field = extrude(&[rectangle(0.0, 0.0, 100.0, 100.0)], 15, 2.0);
        assert_eq!(field.bodies().len(), 1, "shallow plate must still have one body");
        let dmax = field.bodies()[0].dmax;
        assert!(dmax > 0.0 && dmax < 2.0, "ungraded plate must retain its physical maximum depth, got {dmax}");
        assert_eq!(field.norm([50.0, 50.0, 1.5]), 0.0, "the shallow plate with peak {dmax} must have zero normalising depth");
        let cell = field.cell();
        let (_, [nx, ny, nz]) = field.grid();
        for z in 1..nz - 1 {
            for y in 1..ny - 1 {
                for x in 1..nx - 1 {
                    let p = [x, y, z].map(|i| (i as f64 - 0.5) * cell);
                    assert_eq!(field.at(p), (0.0, 1), "plate must be ungraded throughout its body, including {p:?}");
                }
            }
        }
    }

    #[test]
    fn square_tube_excludes_its_bore() {
        let tube = with_hole(rectangle(0.0, 0.0, 40.0, 40.0), 8.0, 32.0);
        let field = extrude(&[tube], 150, 0.0);
        assert_eq!(field.bodies().len(), 1, "four walls of the tube must be connected");
        assert_eq!(field.at([20.0, 20.0, 15.0]), (0.0, 0), "tube bore must have zero grade and outside id");
        assert_eq!(field.depth([20.0, 20.0, 15.0]), 0.0, "depth must vanish in the middle of the bore");
        let (t, id) = field.at([4.0, 20.0, 15.0]);
        assert!(t > 0.0 && id == 1, "solid tube wall must belong to the graded body");
    }

    #[test]
    fn interval_endpoints_gaps_and_edge_contacts() {
        let left = [rectangle(-0.5, 0.0, 0.0, 0.5)];
        let right = [rectangle(0.0, 0.0, 0.5, 0.5)];
        let layers = [(2.0, 2.25, &left[..]), (2.75, 3.5, &left[..]), (3.5, 4.0, &right[..])];
        let field = Depth::new(&layers, 0.25);
        assert_eq!(field.bodies().len(), 3, "neither an uncovered slab nor an edge contact may join bodies");
        assert_eq!(field.at([-0.25, 0.25, 2.25]), (1.0, 1), "closed upper endpoint must fill the slab and grade at the threshold");
        assert_eq!(field.at([-0.25, 0.25, 2.75]), (0.0, 0), "open lower endpoint must leave the gap outside");
        assert_eq!(field.at([-0.25, 0.25, 3.25]), (1.0, 2), "body after the gap must have the next id");
        assert_eq!(field.at([0.25, 0.25, 3.75]), (1.0, 3), "edge-touching voxel must have its own id");
    }
}
