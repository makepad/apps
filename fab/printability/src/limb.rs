//! A limb measured from the occupied sections of a part. Run this on a worker.

use makepad_fab_slicer::geom::ExPolygon;
use std::f64::consts::PI;

#[derive(Debug)]
pub struct LimbFit {
    /// The straight line, directed from start to end.
    pub axis: [f64; 3],
    pub start: [f64; 3],
    pub end: [f64; 3],
    /// How far its sections reach from the line, in mm, without padding: past the last few, where a
    /// section at an end is already what it joins.
    pub radius: f64,
    pub stations: Vec<LimbStation>,
    pub ends: [LimbEnd; 2],
    /// The farthest centre from the line, in mm.
    pub bend: f64,
    /// Evenly thinned section boundaries, in plate mm.
    pub outline: Vec<[f64; 3]>,
    /// Section areas times the station step, in mm³.
    pub volume: f64,
}

#[derive(Debug)]
pub struct LimbStation {
    pub centre: [f64; 3],
    /// The radius of a disc with the section's area.
    pub radius: f64,
    /// The farthest section sample from the centre.
    pub reach: f64,
    section: Section,
    half_step: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LimbEnd {
    Tip,
    /// The first oversized section's radius, capped at its window.
    Meets { radius: f64 },
}

impl LimbFit {
    /// Each cell of its sections, in plate mm: what it is made of, evenly.
    pub fn each_cell(&self, visit: &mut dyn FnMut([f64; 3])) {
        for s in &self.stations {
            for &i in &s.section.cells {
                visit(s.section.point(i));
            }
        }
    }

    /// How far a plate point is off the limb's own rod, its line and radius: negative inside. What's
    /// further off than that is something else, however a section's edge rounds.
    pub fn off_rod(&self, p: [f64; 3]) -> f64 {
        let d = sub(self.end, self.start);
        let t = (dot(sub(p, self.start), d) / norm2(d).max(1e-12)).clamp(0.0, 1.0);
        norm2(sub(p, add(self.start, mul(d, t)))).sqrt() - self.radius
    }

    /// Whether a plate point lies in the section of its nearest station.
    pub fn holds(&self, p: [f64; 3]) -> bool {
        if !p.iter().all(|v| v.is_finite()) {
            return false;
        }
        let Some(s) = self.stations.iter().min_by(|a, b| {
            norm2(sub(p, a.centre)).total_cmp(&norm2(sub(p, b.centre)))
        }) else { return false };
        let d = sub(p, s.section.origin);
        if dot(d, s.section.normal).abs() > s.half_step {
            return false;
        }
        let x = (dot(d, s.section.a) / s.section.cell).round() as isize + s.section.half as isize;
        let y = (dot(d, s.section.b) / s.section.cell).round() as isize + s.section.half as isize;
        s.section.has(x, y)
    }
}

/// The solid of the stack inside a box, cell by cell: `visit` gets each solid cell's middle. What a
/// read counts what's inside a volume with.
pub fn solid_samples<R: AsRef<[ExPolygon]>>(layers: &[(f64, f64, R)], bounds: ([f64; 3], [f64; 3]), cell: f64, stop: &dyn Fn() -> bool, visit: &mut dyn FnMut([f64; 3])) -> Result<(), String> {
    let mut stack = Stack::new(layers, cell);
    let count = |k: usize| ((bounds.1[k] - bounds.0[k]) / cell).ceil().max(0.0) as usize;
    for z in 0..count(2) {
        cancelled(stop)?;
        for y in 0..count(1) {
            for x in 0..count(0) {
                let p = [bounds.0[0] + (x as f64 + 0.5) * cell, bounds.0[1] + (y as f64 + 0.5) * cell, bounds.0[2] + (z as f64 + 0.5) * cell];
                if stack.solid(p) {
                    visit(p);
                }
            }
        }
    }
    Ok(())
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] { std::array::from_fn(|i| a[i] + b[i]) }
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] { std::array::from_fn(|i| a[i] - b[i]) }
fn mul(a: [f64; 3], s: f64) -> [f64; 3] { a.map(|v| v * s) }
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 { (0..3).map(|i| a[i] * b[i]).sum() }
fn norm2(a: [f64; 3]) -> f64 { dot(a, a) }
fn unit(a: [f64; 3]) -> [f64; 3] { mul(a, 1.0 / norm2(a).sqrt()) }
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn cancelled(stop: &dyn Fn() -> bool) -> Result<(), String> {
    if stop() { Err("cancelled".into()) } else { Ok(()) }
}
fn bit(bits: &[u64], i: usize) -> bool { bits[i / 64] & (1 << (i % 64)) != 0 }
fn set(bits: &mut [u64], i: usize) { bits[i / 64] |= 1 << (i % 64); }

struct Raster {
    origin: [f64; 2],
    width: usize,
    height: usize,
    bits: Vec<u64>,
}

struct Stack<'a, R> {
    layers: &'a [(f64, f64, R)],
    cell: f64,
    rasters: Vec<Option<Raster>>,
}

impl<'a, R: AsRef<[ExPolygon]>> Stack<'a, R> {
    fn new(layers: &'a [(f64, f64, R)], cell: f64) -> Self {
        Self { layers, cell, rasters: (0..layers.len()).map(|_| None).collect() }
    }

    fn raster(&self, regions: &[ExPolygon]) -> Raster {
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        for e in regions {
            for p in &e.contour.pts {
                lo[0] = lo[0].min(p.x_mm()); lo[1] = lo[1].min(p.y_mm());
                hi[0] = hi[0].max(p.x_mm()); hi[1] = hi[1].max(p.y_mm());
            }
        }
        if !lo[0].is_finite() {
            return Raster { origin: [0.0; 2], width: 0, height: 0, bits: Vec::new() };
        }
        let origin = lo.map(|v| (v / self.cell).floor() * self.cell);
        let width = ((hi[0] - origin[0]) / self.cell).ceil() as usize;
        let height = ((hi[1] - origin[1]) / self.cell).ceil() as usize;
        let mut out = Raster { origin, width, height, bits: vec![0; (width * height).div_ceil(64)] };
        let mut crossings = Vec::new();
        for e in regions {
            let bbox = e.bbox();
            // Include boundary rows; the crossings decide whether their centres are inside.
            let first = (((bbox.min.y_mm() - origin[1]) / self.cell - 0.5).floor().max(0.0) as usize).min(height);
            let last = ((((bbox.max.y_mm() - origin[1]) / self.cell - 0.5).ceil().max(0.0) as usize) + 1).min(height);
            for row in first..last {
                let y = origin[1] + (row as f64 + 0.5) * self.cell;
                crossings.clear();
                for ring in e.rings() {
                    for i in 0..ring.pts.len() {
                        let a = ring.pts[i];
                        let b = ring.pts[(i + 1) % ring.pts.len()];
                        if (a.y_mm() > y) != (b.y_mm() > y) {
                            crossings.push(a.x_mm() + (y - a.y_mm()) * (b.x_mm() - a.x_mm()) / (b.y_mm() - a.y_mm()));
                        }
                    }
                }
                crossings.sort_by(f64::total_cmp);
                for pair in crossings.chunks_exact(2) {
                    let left = (((pair[0] - origin[0]) / self.cell - 0.5).ceil().max(0.0) as usize).min(width);
                    let right = (((pair[1] - origin[0]) / self.cell - 0.5).ceil().max(0.0) as usize).min(width);
                    for x in left..right { set(&mut out.bits, row * width + x); }
                }
            }
        }
        out
    }

    fn solid(&mut self, p: [f64; 3]) -> bool {
        // A print layer covers the height below its recorded top Z.
        let i = self.layers.partition_point(|l| l.0 < p[2]);
        if i == self.layers.len() || p[2] < self.layers[i].0 - self.layers[i].1 { return false; }
        if self.rasters[i].is_none() { self.rasters[i] = Some(self.raster(self.layers[i].2.as_ref())); }
        let r = self.rasters[i].as_ref().unwrap();
        let x = ((p[0] - r.origin[0]) / self.cell).floor();
        let y = ((p[1] - r.origin[1]) / self.cell).floor();
        x >= 0.0 && y >= 0.0 && x < r.width as f64 && y < r.height as f64
            && bit(&r.bits, y as usize * r.width + x as usize)
    }
}

const SIDE: usize = 64;
/// How much deeper a limb's middle is than a point of it, per mm further in: a corner's deepening
/// line clears this, the way a limb widens into what it joins doesn't.
const DEEPER: f64 = 0.7;

struct Crop {
    origin: [f64; 3],
    cell: f64,
    distances: Vec<f64>,
    centre: usize,
}

impl Crop {
    fn xyz(i: usize) -> [usize; 3] { [i % SIDE, i / SIDE % SIDE, i / (SIDE * SIDE)] }
    fn index(p: [usize; 3]) -> usize { p[0] + SIDE * (p[1] + SIDE * p[2]) }
    fn point(&self, i: usize) -> [f64; 3] {
        let p = Self::xyz(i);
        std::array::from_fn(|k| self.origin[k] + (p[k] as f64 + 0.5) * self.cell)
    }
    fn radius(&self) -> f64 { self.distances[self.centre].sqrt() * self.cell }
}

// Lower envelope of squared-distance parabolas along one grid line.
fn distance_line(f: &[f64; SIDE]) -> [f64; SIDE] {
    let mut sites = [0usize; SIDE];
    let mut cuts = [f64::INFINITY; SIDE + 1];
    cuts[0] = f64::NEG_INFINITY;
    let mut k = 0;
    for q in 1..SIDE {
        let mut s;
        loop {
            let v = sites[k];
            s = (f[q] + (q * q) as f64 - f[v] - (v * v) as f64) / (2.0 * (q - v) as f64);
            if s > cuts[k] { break; }
            k -= 1;
        }
        k += 1; sites[k] = q; cuts[k] = s; cuts[k + 1] = f64::INFINITY;
    }
    k = 0;
    std::array::from_fn(|q| {
        while cuts[k + 1] < q as f64 { k += 1; }
        let d = q as f64 - sites[k] as f64;
        d * d + f[sites[k]]
    })
}

fn crop<R: AsRef<[ExPolygon]>>(layers: &[(f64, f64, R)], p: [f64; 3], extent: f64, stop: &dyn Fn() -> bool) -> Result<Crop, String> {
    let mut size = 16.0;
    loop {
        cancelled(stop)?;
        let cell = size / SIDE as f64;
        let mut stack = Stack::new(layers, cell);
        let mut out = Crop { origin: p.map(|v| v - size / 2.0), cell, distances: vec![0.0; SIDE.pow(3)], centre: 0 };
        for i in 0..out.distances.len() {
            if stack.solid(out.point(i)) {
                // Cells outside the crop are empty; an edge hit grows the crop.
                let edge = Crop::xyz(i).into_iter().map(|v| (v + 1).min(SIDE - v)).min().unwrap();
                out.distances[i] = (edge * edge) as f64;
            }
        }
        let nearest = (0..out.distances.len()).filter(|&i| out.distances[i] > 0.0)
            .map(|i| (i, norm2(sub(out.point(i), p)).sqrt())).min_by(|a, b| a.1.total_cmp(&b.1));
        let Some((start, nearest)) = nearest.filter(|n| n.1 <= 2.0 * cell) else {
            return Err(format!("point {p:?} is on no layer of the part"));
        };
        for axis in 0..3 {
            cancelled(stop)?;
            for a in 0..SIDE {
                for b in 0..SIDE {
                    let indices: [usize; SIDE] = std::array::from_fn(|t| {
                        let mut xyz = [0; 3]; xyz[axis] = t; xyz[(axis + 1) % 3] = a; xyz[(axis + 2) % 3] = b;
                        Crop::index(xyz)
                    });
                    let line = distance_line(&indices.map(|i| out.distances[i]));
                    for (i, d) in indices.into_iter().zip(line) { out.distances[i] = d; }
                }
            }
        }
        // The middle: the biggest ball inside the part that reaches the point and is deeper the
        // further in it is, what a surface point has one of however its facets fall on the grid. What
        // only widens along a rod, from a point in it, is the rod's next part. A rod has a run of
        // them, as big to half a cell: the one level with the point.
        let from = out.point(start);
        let depth = out.distances[start].sqrt();
        let cells = out.distances.iter().enumerate().filter(|(_, d)| **d > 0.0).map(|(i, d)| (i, d.sqrt(), norm2(sub(out.point(i), p)).sqrt()));
        let reaching: Vec<_> = cells.filter(|(i, d, off)| {
            *off <= d * cell + nearest && (d - depth) * cell >= DEEPER * norm2(sub(out.point(*i), from)).sqrt() - cell
        }).collect();
        let biggest = reaching.iter().map(|c| c.1).fold(0.0, f64::max);
        out.centre = reaching.iter().filter(|c| c.1 >= biggest - 0.5).min_by(|a, b| a.2.total_cmp(&b.2)).map_or(start, |c| c.0);
        // past this its ball could run into the crop's edge, where everything reads as empty
        if out.radius() > size / 5.0 && size < extent {
            size *= 2.0;
            continue;
        }
        return Ok(out);
    }
}

fn principal(points: &[[f64; 3]]) -> Result<([f64; 3], [f64; 3]), String> {
    if points.len() < 2 { return Err("too few limb centres for an axis".into()); }
    let mean = mul(points.iter().copied().fold([0.0; 3], add), 1.0 / points.len() as f64);
    let mut m = [[0.0; 3]; 3];
    for &p in points {
        let p = sub(p, mean);
        for i in 0..3 { for j in 0..3 { m[i][j] += p[i] * p[j]; } }
    }
    let mut best = [0.0; 3];
    let mut value = 0.0;
    for seed in 0..3 {
        let mut v = [0.0; 3]; v[seed] = 1.0;
        // Power iteration from each basis vector also covers diagonal matrices.
        for _ in 0..64 {
            let w = m.map(|row| dot(row, v));
            if norm2(w) == 0.0 { break; }
            let next = unit(w);
            let converged = norm2(sub(next, v)) < 1e-20;
            v = next;
            if converged { break; }
        }
        let eigenvalue = dot(v, m.map(|row| dot(row, v)));
        if eigenvalue > value { value = eigenvalue; best = v; }
    }
    if value == 0.0 { return Err("limb centres do not define an axis".into()); }
    Ok((mean, best))
}

#[derive(Debug)]
struct Section {
    origin: [f64; 3],
    normal: [f64; 3],
    a: [f64; 3],
    b: [f64; 3],
    cell: f64,
    half: usize,
    bits: Vec<u64>,
    cells: Vec<usize>,
    border: bool,
}

impl Section {
    fn width(&self) -> usize { 2 * self.half + 1 }
    fn point(&self, i: usize) -> [f64; 3] {
        let x = (i % self.width()) as f64 - self.half as f64;
        let y = (i / self.width()) as f64 - self.half as f64;
        add(self.origin, add(mul(self.a, x * self.cell), mul(self.b, y * self.cell)))
    }
    fn has(&self, x: isize, y: isize) -> bool {
        x >= 0 && y >= 0 && x < self.width() as isize && y < self.width() as isize
            && bit(&self.bits, y as usize * self.width() + x as usize)
    }
    fn area(&self) -> f64 { self.cells.len() as f64 * self.cell * self.cell }
    fn boundary(&self, i: usize) -> bool {
        let x = (i % self.width()) as isize;
        let y = (i / self.width()) as isize;
        !self.has(x - 1, y) || !self.has(x + 1, y) || !self.has(x, y - 1) || !self.has(x, y + 1)
    }
    fn station(self, step: f64) -> LimbStation {
        let centre = mul(self.cells.iter().map(|&i| self.point(i)).fold([0.0; 3], add), 1.0 / self.cells.len() as f64);
        let reach = self.cells.iter().map(|&i| norm2(sub(self.point(i), centre))).fold(0.0, f64::max).sqrt();
        LimbStation { centre, radius: (self.area() / PI).sqrt(), reach, section: self, half_step: step / 2.0 }
    }
}

fn section<R: AsRef<[ExPolygon]>>(stack: &mut Stack<'_, R>, q: [f64; 3], u: [f64; 3], reference: f64) -> Section {
    let seed_axis = if u[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [0.0, 1.0, 0.0] };
    let a = unit(cross(u, seed_axis));
    let b = cross(u, a);
    let half = (3.0 * reference / stack.cell).ceil() as usize;
    let width = 2 * half + 1;
    let mut out = Section { origin: q, normal: u, a, b, cell: stack.cell, half,
        bits: vec![0; (width * width).div_ceil(64)], cells: Vec::new(), border: false };
    let mut occupied = vec![false; width * width];
    let mut nearest = None;
    let mut distance = (reference / 2.0).powi(2);
    for (i, solid) in occupied.iter_mut().enumerate() {
        *solid = stack.solid(out.point(i));
        if *solid {
            let d = norm2(sub(out.point(i), q));
            if d <= distance { nearest = Some(i); distance = d; }
        }
    }
    let Some(i) = nearest else { return out };
    out.cells.push(i); set(&mut out.bits, i);
    let mut next = 0;
    while next < out.cells.len() {
        let i = out.cells[next]; next += 1;
        let x = i % width; let y = i / width;
        if x == 0 || y == 0 || x + 1 == width || y + 1 == width { out.border = true; }
        for (dx, dy) in [(-1isize, 0isize), (1, 0), (0, -1), (0, 1)] {
            let nx = x as isize + dx; let ny = y as isize + dy;
            if nx < 0 || ny < 0 || nx >= width as isize || ny >= width as isize { continue; }
            let j = ny as usize * width + nx as usize;
            if occupied[j] && !bit(&out.bits, j) { set(&mut out.bits, j); out.cells.push(j); }
        }
    }
    out
}

fn median(stations: &[LimbStation], initial: f64) -> f64 {
    if stations.is_empty() { return initial; }
    let mut radii: Vec<f64> = stations.iter().rev().take(5).map(|s| s.radius).collect();
    radii.sort_by(f64::total_cmp);
    radii[radii.len() / 2]
}

fn march<R: AsRef<[ExPolygon]>>(stack: &mut Stack<'_, R>, mut q: [f64; 3], mut u: [f64; 3], radius: f64, step: f64, stop: &dyn Fn() -> bool) -> Result<(Vec<LimbStation>, LimbEnd), String> {
    let mut stations = Vec::new();
    loop {
        cancelled(stop)?;
        let reference = median(&stations, radius);
        let s = section(stack, q, u, reference);
        if s.border { return Ok((stations, LimbEnd::Meets { radius: 3.0 * reference })); }
        if s.area() < PI * reference * reference / 4.0 { return Ok((stations, LimbEnd::Tip)); }
        let r = (s.area() / PI).sqrt();
        if r > 1.5 * reference { return Ok((stations, LimbEnd::Meets { radius: r.min(3.0 * reference) })); }
        stations.push(s.station(step));
        if stations.len() >= 3 {
            let points: Vec<_> = stations.iter().rev().take(5).map(|s| s.centre).collect();
            let (_, mut direction) = principal(&points)?;
            if dot(direction, u) < 0.0 { direction = mul(direction, -1.0); }
            u = direction;
        }
        q = add(stations.last().unwrap().centre, mul(u, step));
    }
}

#[derive(Clone, Copy)]
struct Guide { centre: [f64; 3], radius: f64 }

fn walk<R: AsRef<[ExPolygon]>>(stack: &mut Stack<'_, R>, guides: &[Guide], axis: [f64; 3], step: f64, stop: &dyn Fn() -> bool) -> Result<(Vec<LimbStation>, [LimbEnd; 2]), String> {
    let first = guides[0];
    let last = guides[guides.len() - 1];
    let (mut before, a) = march(stack, first.centre, mul(axis, -1.0), first.radius, step, stop)?;
    before.reverse();
    let mut stations = before;
    if guides.len() > 1 {
        stations.pop();
        for pair in guides.windows(2) {
            let delta = sub(pair[1].centre, pair[0].centre);
            let length = norm2(delta).sqrt();
            let direction = unit(delta);
            let n = (length / step).ceil() as usize;
            let span_step = length / n as f64;
            let reference = pair[0].radius.max(pair[1].radius);
            for i in 0..n {
                cancelled(stop)?;
                let q = add(pair[0].centre, mul(direction, i as f64 * span_step));
                let s = section(stack, q, direction, reference);
                if s.cells.is_empty() || s.border { return Err("picked points do not share a limb section".into()); }
                stations.push(s.station(span_step));
            }
        }
    } else {
        stations.pop();
    }
    let (mut after, b) = march(stack, last.centre, axis, last.radius, step, stop)?;
    stations.append(&mut after);
    Ok((stations, [a, b]))
}

/// How many ways a limb's middle is tried: about every 13 degrees.
const WAYS: usize = 96;

/// Which way a limb runs at its middle: across its smallest section. The longest line of the solid
/// round a short neck runs along what it joins.
fn limb_way<R: AsRef<[ExPolygon]>>(stack: &mut Stack<'_, R>, middle: Guide, stop: &dyn Fn() -> bool) -> Result<[f64; 3], String> {
    let mut best = ([0.0, 0.0, 1.0], f64::INFINITY);
    for i in 0..WAYS {
        cancelled(stop)?;
        let up = (i as f64 + 0.5) / WAYS as f64;
        let round = (1.0 - up * up).sqrt();
        let turn = i as f64 * PI * (3.0 - 5f64.sqrt());
        let way = [round * turn.cos(), round * turn.sin(), up];
        let area = section(stack, middle.centre, way, middle.radius).area();
        if area > 0.0 && area < best.1 { best = (way, area); }
    }
    Ok(best.0)
}

/// Fit a limb from surface points in a part's layer stack, in plate mm.
pub fn fit_limb<R: AsRef<[ExPolygon]>>(layers: &[(f64, f64, R)], points: &[[f64; 3]], stop: &dyn Fn() -> bool) -> Result<LimbFit, String> {
    cancelled(stop)?;
    if points.is_empty() { return Err("pick at least one point on the part".into()); }
    if points.iter().flatten().any(|v| !v.is_finite()) { return Err("picked point is not finite".into()); }
    if layers.iter().any(|l| !l.0.is_finite() || !l.1.is_finite() || l.1 <= 0.0)
        || layers.windows(2).any(|p| p[0].0 >= p[1].0) {
        return Err("layers must have increasing finite top heights and positive heights".into());
    }
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for (z, h, regions) in layers {
        for e in regions.as_ref() {
            for p in &e.contour.pts {
                let p = [p.x_mm(), p.y_mm(), *z];
                for k in 0..3 { lo[k] = lo[k].min(p[k]); hi[k] = hi[k].max(p[k]); }
                lo[2] = lo[2].min(z - h);
            }
        }
    }
    let mut guides = Vec::new();
    for &p in points {
        let extent = (0..3).map(|k| (p[k] - lo[k]).abs().max((hi[k] - p[k]).abs()) * 2.0).fold(16.0, f64::max);
        let crop = crop(layers, p, extent, stop)?;
        guides.push(Guide { centre: crop.point(crop.centre), radius: crop.radius() });
    }
    let radius = guides[0].radius;
    let cell = (radius / 10.0).clamp(0.1, 0.4);
    let step = cell.max(radius / 3.0);
    let mut stack = Stack::new(layers, cell);
    let mut axis = if guides.len() == 1 {
        limb_way(&mut stack, guides[0], stop)?
    } else {
        let d = sub(guides.last().unwrap().centre, guides[0].centre);
        if norm2(d) < cell * cell { return Err("picked centres are too close to define an axis".into()); }
        unit(d)
    };
    let (first, _) = walk(&mut stack, &guides, axis, step, stop)?;
    if first.len() < 2 { return Err(format!("no limb runs through {:?}: its sections are as wide as what is round them", points[0])); }
    let centres: Vec<_> = first.iter().map(|s| s.centre).collect();
    let (_, fitted) = principal(&centres)?;
    axis = if dot(fitted, axis) < 0.0 { mul(fitted, -1.0) } else { fitted };
    if guides.len() == 1 {
        let middle = &first[first.len() / 2];
        guides[0] = Guide { centre: middle.centre, radius: middle.radius };
    } else {
        // A crop's ball can stop short of the limb's middle: the sections say how thick it is here.
        for guide in &mut guides {
            let near = first.iter().min_by(|a, b| norm2(sub(a.centre, guide.centre)).total_cmp(&norm2(sub(b.centre, guide.centre)))).unwrap();
            *guide = Guide { centre: near.centre, radius: near.radius };
        }
    }
    let (stations, ends) = walk(&mut stack, &guides, axis, step, stop)?;
    let centres: Vec<_> = stations.iter().map(|s| s.centre).collect();
    let (origin, fitted) = principal(&centres)?;
    axis = if dot(fitted, axis) < 0.0 { mul(fitted, -1.0) } else { fitted };
    let projection = |p| dot(sub(p, origin), axis);
    let off_line = |p| norm2(sub(sub(p, origin), mul(axis, projection(p)))).sqrt();
    let mut start = projection(stations[0].centre);
    let mut end = projection(stations.last().unwrap().centre);
    let mut reaches = Vec::with_capacity(stations.len());
    let mut bend: f64 = 0.0;
    let mut volume = 0.0;
    let mut outline = Vec::new();
    for s in &stations {
        bend = bend.max(off_line(s.centre));
        volume += s.section.area() * s.half_step * 2.0;
        let mut reach: f64 = 0.0;
        for &i in &s.section.cells {
            let p = s.section.point(i);
            reach = reach.max(off_line(p));
            if ends[0] == LimbEnd::Tip { start = start.min(projection(p)); }
            if ends[1] == LimbEnd::Tip { end = end.max(projection(p)); }
            if s.section.boundary(i) { outline.push(p); }
        }
        reaches.push(reach);
    }
    reaches.sort_by(f64::total_cmp);
    let radius = reaches[(reaches.len() - 1) * 9 / 10];
    if outline.len() > 4000 {
        outline = (0..4000).map(|i| outline[i * outline.len() / 4000]).collect();
    }
    Ok(LimbFit { axis, start: add(origin, mul(axis, start)), end: add(origin, mul(axis, end)), radius, stations, ends, bend, outline, volume })
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_fab_slicer::geom::{Polygon, Pt};

    enum Shape {
        Box { lo: [f64; 3], hi: [f64; 3] },
        Tube { base: [f64; 3], axis: [f64; 3], length: f64, radii: [f64; 2] },
        Ball { centre: [f64; 3], radius: f64 },
    }

    impl Shape {
        fn bounds(&self) -> ([f64; 3], [f64; 3]) {
            match *self {
                Self::Box { lo, hi } => (lo, hi),
                Self::Ball { centre, radius } => (centre.map(|v| v - radius), centre.map(|v| v + radius)),
                Self::Tube { base, axis, length, radii } => {
                    let end = add(base, mul(axis, length));
                    let radius = radii[0].max(radii[1]);
                    (std::array::from_fn(|k| base[k].min(end[k]) - radius), std::array::from_fn(|k| base[k].max(end[k]) + radius))
                }
            }
        }
        fn row(&self, y: f64, z: f64) -> Vec<(f64, f64)> {
            match *self {
                Self::Box { lo, hi } => {
                    if y >= lo[1] && y < hi[1] && z >= lo[2] && z < hi[2] { vec![(lo[0], hi[0])] } else { Vec::new() }
                }
                Self::Ball { centre, radius } => {
                    let r2 = radius * radius - (y - centre[1]).powi(2) - (z - centre[2]).powi(2);
                    if r2 > 0.0 { vec![(centre[0] - r2.sqrt(), centre[0] + r2.sqrt())] } else { Vec::new() }
                }
                Self::Tube { base, axis, length, radii } => {
                    let dy = y - base[1]; let dz = z - base[2];
                    let t0 = axis[1] * dy + axis[2] * dz;
                    let slope = (radii[1] - radii[0]) / length;
                    let r = radii[0] + slope * t0;
                    let a = 1.0 - axis[0].powi(2) * (1.0 + slope * slope);
                    let b = -2.0 * axis[0] * (t0 + slope * r);
                    let c = dy * dy + dz * dz - t0 * t0 - r * r;
                    let (lo, hi) = self.bounds();
                    let mut cuts = vec![lo[0] - base[0], hi[0] - base[0]];
                    if axis[0].abs() > 1e-12 { cuts.extend([-t0 / axis[0], (length - t0) / axis[0]]); }
                    if a.abs() < 1e-12 {
                        if b.abs() > 1e-12 { cuts.push(-c / b); }
                    } else {
                        let d = b * b - 4.0 * a * c;
                        if d >= 0.0 { cuts.extend([(-b - d.sqrt()) / (2.0 * a), (-b + d.sqrt()) / (2.0 * a)]); }
                    }
                    cuts.retain(|x| *x >= lo[0] - base[0] && *x <= hi[0] - base[0]);
                    cuts.sort_by(f64::total_cmp);
                    let mut out = Vec::new();
                    for pair in cuts.windows(2) {
                        let x = (pair[0] + pair[1]) / 2.0;
                        let t = t0 + axis[0] * x;
                        if t >= 0.0 && t <= length && a * x * x + b * x + c <= 0.0 {
                            out.push((pair[0] + base[0], pair[1] + base[0]));
                        }
                    }
                    out
                }
            }
        }
    }

    fn rectangle(x0: f64, y0: f64, x1: f64, y1: f64) -> ExPolygon {
        ExPolygon::new(Polygon { pts: vec![Pt::from_mm(x0, y0), Pt::from_mm(x1, y0), Pt::from_mm(x1, y1), Pt::from_mm(x0, y1)] })
    }

    fn stack(shapes: &[Shape]) -> Vec<(f64, f64, Vec<ExPolygon>)> {
        let low = shapes.iter().map(|s| s.bounds().0[2]).fold(f64::INFINITY, f64::min);
        let high = shapes.iter().map(|s| s.bounds().1[2]).fold(f64::NEG_INFINITY, f64::max);
        ((low / 0.2).floor() as i32..(high / 0.2).ceil() as i32).map(|layer| {
            let z = (layer as f64 + 0.5) * 0.2;
            let mut regions = Vec::new();
            for shape in shapes {
                let (lo, hi) = shape.bounds();
                if z < lo[2] || z >= hi[2] { continue; }
                if let Shape::Box { lo, hi } = *shape {
                    regions.push(rectangle(lo[0], lo[1], hi[0], hi[1]));
                    continue;
                }
                for row in (lo[1] / 0.1).floor() as i32..(hi[1] / 0.1).ceil() as i32 {
                    let y = (row as f64 + 0.5) * 0.1;
                    for (left, right) in shape.row(y, z) {
                        let left = (left / 0.1 - 0.5).ceil() * 0.1;
                        let right = (right / 0.1 - 0.5).ceil() * 0.1;
                        if right > left { regions.push(rectangle(left, row as f64 * 0.1, right, (row + 1) as f64 * 0.1)); }
                    }
                }
            }
            ((layer + 1) as f64 * 0.2, 0.2, regions)
        }).collect()
    }

    fn tube(base: [f64; 3], axis: [f64; 3], length: f64, radii: [f64; 2]) -> Shape {
        Shape::Tube { base, axis, length, radii }
    }
    fn torso() -> Shape { Shape::Box { lo: [-20.0, -12.0, 0.0], hi: [20.0, 12.0, 80.0] } }
    fn end_at(fit: &LimbFit, meets: bool) -> [f64; 3] {
        if matches!(fit.ends[0], LimbEnd::Meets { .. }) == meets { fit.start } else { fit.end }
    }

    #[test]
    fn limb_straight_orientations_and_holds() {
        let angle = 35.0f64.to_radians();
        for (base, axis) in [([20.0, 0.0, 40.0], [angle.cos(), 0.0, angle.sin()]),
            ([20.0, 0.0, 40.0], [1.0, 0.0, 0.0]), ([0.0, 0.0, 80.0], [0.0, 0.0, 1.0])] {
            let layers = stack(&[torso(), tube(base, axis, 60.0, [5.0; 2])]);
            let middle = add(base, mul(axis, 30.0));
            let fit = fit_limb(&layers, &[add(middle, [0.0, 5.0, 0.0])], &|| false).unwrap();
            assert!(dot(fit.axis, axis).abs() > 3.0f64.to_radians().cos(), "axis: {:?}", fit.axis);
            assert!((fit.radius - 5.0).abs() < 0.4, "radius: {}, axis {:?}, start {:?}, end {:?}, bend {}", fit.radius, fit.axis, fit.start, fit.end, fit.bend);
            assert_eq!(fit.ends.iter().filter(|e| matches!(e, LimbEnd::Tip)).count(), 1, "ends: {:?}", fit.ends);
            assert!(dot(sub(end_at(&fit, true), base), axis).abs() <= 5.0, "joint: {:?}", end_at(&fit, true));
            assert!(fit.holds(middle), "axis point held");
            assert!(!fit.holds(sub(base, mul(axis, 10.0))), "torso point excluded");
            assert!(fit.outline.len() <= 4000 && fit.volume > 0.0, "outline and volume");
        }
    }

    #[test]
    fn limb_flare_and_taper() {
        let base = [20.0, 0.0, 40.0]; let axis = [1.0, 0.0, 0.0];
        let layers = stack(&[torso(), tube(base, axis, 6.0, [9.0, 5.0]), tube(add(base, [6.0, 0.0, 0.0]), axis, 54.0, [5.0; 2])]);
        let fit = fit_limb(&layers, &[[50.0, 5.0, 40.0]], &|| false).unwrap();
        let end = end_at(&fit, true);
        assert!(end[0] >= 20.0 && end[0] <= 26.0, "flare end: {end:?}, {:?}", fit.ends);
        let layers = stack(&[tube([0.0, 0.0, 20.0], axis, 60.0, [4.0, 7.0])]);
        let fit = fit_limb(&layers, &[[30.0, 5.5, 20.0]], &|| false).unwrap();
        assert_eq!(fit.ends, [LimbEnd::Tip; 2]);
        assert!(norm2(sub(fit.end, fit.start)).sqrt() > 57.0, "taper length");
        assert!((fit.radius - 7.0).abs() < 0.4, "taper radius: {}", fit.radius);
    }

    #[test]
    fn limb_hand_and_multiple_picks() {
        let layers = stack(&[tube([0.0, 0.0, 20.0], [1.0, 0.0, 0.0], 60.0, [5.0; 2]), Shape::Ball { centre: [60.0, 0.0, 20.0], radius: 10.0 }]);
        let forearm = [30.0, 5.0, 20.0];
        let fit = fit_limb(&layers, &[forearm], &|| false).unwrap();
        assert!(fit.ends.iter().any(|e| matches!(e, LimbEnd::Meets { .. })), "hand stop: {:?}", fit.ends);
        let fit = fit_limb(&layers, &[forearm, [70.0, 0.0, 20.0]], &|| false).unwrap();
        assert!(fit.end[0] >= 68.0, "far side of hand: {:?}", fit.end);
        assert!(fit.holds([60.0, 0.0, 20.0]), "hand centre held");
    }

    #[test]
    fn limb_a_short_neck_fits_from_any_pick_round_it() {
        // a pick on a mesh's facets climbs to the neck's middle, however they fall on the grid
        let mut layers = stack(&[tube([-18.0, 0.0, 112.0], [1.0, 0.0, 0.0], 36.0, [7.0; 2]),
            Shape::Ball { centre: [0.0, 0.0, 132.0], radius: 6.0 }, Shape::Ball { centre: [0.0, 0.0, 140.0], radius: 12.0 }]);
        for sides in [5, 9] {
            for turn in [0.0, 0.37] {
                for layer in &mut layers {
                    layer.2.truncate(layer.2.iter().position(|e| e.contour.pts.len() != 4).unwrap_or(layer.2.len()));
                    if layer.0 > 116.0 && layer.0 < 129.0 {
                        let corner = |k: usize| { let a = (k as f64 + turn) * 2.0 * PI / sides as f64; Pt::from_mm(3.4 * a.cos(), 3.4 * a.sin()) };
                        layer.2.push(ExPolygon::new(Polygon { pts: (0..sides).map(corner).collect() }));
                    }
                }
                let inner = 3.4 * (PI / sides as f64).cos();
                for k in 0..8 {
                    let angle = k as f64 * PI / 4.0;
                    let pick = [inner * angle.cos(), inner * angle.sin(), 120.5];
                    let middle = crop(&layers, pick, 200.0, &|| false).unwrap();
                    assert!(middle.radius() > 0.7 * inner, "{sides} sides, {pick:?}: stopped at {:?}, {} from the surface", middle.point(middle.centre), middle.radius());
                    let fit = fit_limb(&layers, &[pick], &|| false).unwrap_or_else(|e| panic!("{sides} sides, {pick:?}: {e}"));
                    assert!(fit.axis[2].abs() > 10f64.to_radians().cos(), "{sides} sides, {pick:?}: axis {:?}", fit.axis);
                }
                // from its own middle, close under what it widens into, it stays the neck
                let fit = fit_limb(&layers, &[[0.0, 0.0, 124.5]], &|| false).unwrap();
                assert!(fit.axis[2].abs() > 10f64.to_radians().cos(), "{sides} sides: axis {:?}", fit.axis);
                assert!(fit.radius < 2.0 * inner, "{sides} sides: radius {}", fit.radius);
                let (low, high) = (fit.start[2].min(fit.end[2]), fit.start[2].max(fit.end[2]));
                assert!(low > 110.0 && high < 131.0, "{sides} sides: runs {low} to {high}");
            }
        }
    }

    #[test]
    fn limb_bend_and_flat_face() {
        let angle = 60.0f64.to_radians();
        let layers = stack(&[tube([0.0, 0.0, 20.0], [1.0, 0.0, 0.0], 30.0, [5.0; 2]),
            tube([30.0, 0.0, 20.0], [angle.cos(), angle.sin(), 0.0], 30.0, [5.0; 2]),
            Shape::Ball { centre: [30.0, 0.0, 20.0], radius: 5.0 }]);
        let fit = fit_limb(&layers, &[[15.0, 0.0, 25.0]], &|| false).unwrap();
        assert!(fit.bend > fit.radius / 2.0, "bend {}, radius {}", fit.bend, fit.radius);
        let layers = stack(&[torso()]);
        let fit = fit_limb(&layers, &[[20.0, 0.0, 40.0]], &|| false).unwrap();
        assert!(fit.radius >= 10.0, "flat face radius: {}", fit.radius);
    }

    #[test]
    fn limb_air_cancellation_and_occupancy() {
        let layers = stack(&[tube([0.0, 0.0, 20.0], [1.0, 0.0, 0.0], 60.0, [5.0; 2])]);
        let p = [30.0, 50.0, 20.0];
        let error = fit_limb(&layers, &[p], &|| false).unwrap_err();
        assert!(error.contains(&format!("{p:?}")) && error.contains("on no layer"), "{error}");
        assert_eq!(fit_limb(&layers, &[[30.0, 5.0, 20.0]], &|| true).unwrap_err(), "cancelled");
        let mut ring = rectangle(0.0, 0.0, 10.0, 10.0);
        ring.holes.push(rectangle(2.0, 2.0, 8.0, 8.0).contour);
        let layers = vec![(2.0, 0.2, vec![ring, rectangle(4.0, 4.0, 6.0, 6.0)])];
        let mut raster = Stack::new(&layers, 0.1);
        assert!(raster.solid([1.0, 1.0, 1.9]), "contour occupied");
        assert!(!raster.solid([3.0, 3.0, 1.9]), "hole empty");
        assert!(raster.solid([5.0, 5.0, 1.9]), "regions OR across holes");
        assert!(!raster.solid([1.0, 1.0, 1.7]) && !raster.solid([1.0, 1.0, 2.1]), "layer height");
    }
}
