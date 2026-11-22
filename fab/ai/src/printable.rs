//! Mesh repair in Fab's millimetre, Y-up model frame.
//!
//! Call this on a worker: rasterisation and distance transforms use scoped slabs,
//! and no work is attached to the UI event loop. Only an outside flood decides
//! what is solid; winding is used solely for statistics and pose scoring.

use makepad_csg_math::Vec3d;
use makepad_csg_sdf::mesh_field::{closest_on_tri, MeshData};
use makepad_csg_sdf::{surface_mesh::mesh_surface, Clearance, Sdf3};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Options {
    /// Nozzle diameter, also used for the pose scorer's bed-contact tolerance.
    pub nozzle_mm: f64,
    /// Features thinner than this are thickened locally, in finished millimetres.
    pub min_feature_mm: f64,
    /// None selects a cell from surface area, triangle budget and grid storage.
    pub cell_mm: Option<f64>,
    pub max_triangles: usize,
    pub orient: bool,
    /// Requested Y extent after orientation, repair and the base cut.
    pub height_mm: Option<f64>,
    pub flat_base: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { nozzle_mm: 0.4, min_feature_mm: 0.8, cell_mm: None, max_triangles: 120_000, orient: true, height_mm: None, flat_base: true }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub before: Stats,
    pub after: Stats,
    pub closing_radius_mm: f64,
    pub cell_mm: f64,
    pub loops_capped: usize,
    pub islands_removed: usize,
    pub thickened_mm3: f64,
    pub base_cut_mm: f64,
    pub base_area_mm2: f64,
    pub rotated: Option<([f64; 3], f64)>,
    pub overhang_before_mm2: f64,
    pub overhang_after_mm2: f64,
    pub scale: f64,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub triangles: usize,
    pub open_edges: usize,
    pub nonmanifold_edges: usize,
    pub shells: usize,
    pub volume_mm3: f64,
    pub size: [f64; 3],
}

type Point = [f64; 3];
type Edge = (u32, u32);

fn add(a: Point, b: Point) -> Point { std::array::from_fn(|k| a[k] + b[k]) }
fn sub(a: Point, b: Point) -> Point { std::array::from_fn(|k| a[k] - b[k]) }
fn mul(a: Point, s: f64) -> Point { a.map(|x| x * s) }
fn dot(a: Point, b: Point) -> f64 { (0..3).map(|k| a[k] * b[k]).sum() }
fn length(a: Point) -> f64 { dot(a, a).sqrt() }
fn cross(a: Point, b: Point) -> Point {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn unit(a: Point) -> Point { mul(a, 1.0 / length(a).max(f64::MIN_POSITIVE)) }
fn vec3(p: Point) -> Vec3d { Vec3d::new(p[0], p[1], p[2]) }

fn bounds(positions: &[[f32; 3]]) -> (Point, Point) {
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for p in positions {
        for k in 0..3 { lo[k] = lo[k].min(p[k] as f64); hi[k] = hi[k].max(p[k] as f64); }
    }
    (lo, hi)
}

fn triangle(mesh: &MeshData, t: [u32; 3]) -> [Point; 3] { t.map(|v| mesh.positions[v as usize].map(f64::from)) }
fn normal(p: [Point; 3]) -> Point { cross(sub(p[1], p[0]), sub(p[2], p[0])) }

fn edge_counts(mesh: &MeshData) -> HashMap<Edge, usize> {
    let mut edges = HashMap::with_capacity(mesh.triangles.len() * 2);
    for t in &mesh.triangles {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *edges.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    edges
}

struct Sets { parent: Vec<usize>, size: Vec<usize> }

impl Sets {
    fn new(n: usize) -> Self { Self { parent: (0..n).collect(), size: vec![1; n] } }
    fn push(&mut self) { self.parent.push(self.parent.len()); self.size.push(1); }
    fn root(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i { self.parent[i] = self.parent[self.parent[i]]; i = self.parent[i]; }
        i
    }
    fn join(&mut self, a: usize, b: usize) {
        let (mut a, mut b) = (self.root(a), self.root(b));
        if a == b { return; }
        if self.size[a] < self.size[b] { std::mem::swap(&mut a, &mut b); }
        self.parent[b] = a;
        self.size[a] += self.size[b];
    }
}

/// Statistics on the indexed surface. Invalid faces are omitted; vertex seams
/// are welded by `make_printable` before it records its input statistics.
/// Volume is the absolute signed volume, an estimate on open or flipped input.
pub fn stats(mesh: &MeshData) -> Stats {
    let mut sets = Sets::new(mesh.positions.len());
    let mut used = vec![false; mesh.positions.len()];
    let mut edges: HashMap<Edge, usize> = HashMap::new();
    let mut out = Stats::default();
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    let origin = mesh.positions.iter().find(|p| p.iter().all(|x| x.is_finite())).copied().unwrap_or([0.0; 3]).map(f64::from);
    let mut volume = 0.0;
    for &t in &mesh.triangles {
        if t.iter().any(|&v| v as usize >= mesh.positions.len()) { continue; }
        let p = triangle(mesh, t);
        if p.iter().flatten().any(|x| !x.is_finite()) || length(normal(p)) == 0.0 { continue; }
        out.triangles += 1;
        volume += dot(sub(p[0], origin), cross(sub(p[1], origin), sub(p[2], origin))) / 6.0;
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *edges.entry((a.min(b), a.max(b))).or_default() += 1;
            sets.join(a as usize, b as usize);
            used[a as usize] = true;
            for d in 0..3 { lo[d] = lo[d].min(p[k][d]); hi[d] = hi[d].max(p[k][d]); }
        }
    }
    out.open_edges = edges.values().filter(|&&n| n == 1).count();
    out.nonmanifold_edges = edges.values().filter(|&&n| n > 2).count();
    out.shells = (0..used.len()).filter(|&i| used[i] && sets.root(i) == i).count();
    out.volume_mm3 = volume.abs();
    if out.triangles != 0 { out.size = sub(hi, lo); }
    out
}

/// The same quantised weld as `img2mesh::fit`, without sheet removal or scaling.
fn weld(mesh: &MeshData) -> Result<MeshData, String> {
    if mesh.triangles.is_empty() { return Err("the mesh has no triangles".into()); }
    if mesh.positions.iter().flatten().any(|x| !x.is_finite()) { return Err("the mesh has a non-finite position".into()); }
    if mesh.triangles.iter().flatten().any(|&v| v as usize >= mesh.positions.len()) { return Err("a triangle names a missing vertex".into()); }
    let (lo, hi) = bounds(&mesh.positions);
    let extent = length(sub(hi, lo));
    if extent <= 0.0 { return Err("the mesh has no size".into()); }
    let q = sub(hi, lo).into_iter().fold(0.0, f64::max) * 1e-6;
    let mut index = HashMap::new();
    let mut out = MeshData::default();
    for &t in &mesh.triangles {
        let t = t.map(|v| {
            let p = mesh.positions[v as usize];
            let key: [i64; 3] = std::array::from_fn(|k| ((p[k] as f64 - lo[k]) / q).round() as i64);
            *index.entry(key).or_insert_with(|| { let v = out.positions.len() as u32; out.positions.push(p); v })
        });
        if t[0] != t[1] && t[1] != t[2] && t[2] != t[0] && length(normal(triangle(&out, t))) > 0.0 { out.triangles.push(t); }
    }
    compact(&mut out);
    if out.triangles.is_empty() { return Err("the mesh has only degenerate triangles".into()); }
    Ok(out)
}

fn compact(mesh: &mut MeshData) {
    let mut remap = vec![u32::MAX; mesh.positions.len()];
    let mut positions = Vec::new();
    for t in &mut mesh.triangles {
        for v in t {
            let old = *v as usize;
            if remap[old] == u32::MAX { remap[old] = positions.len() as u32; positions.push(mesh.positions[old]); }
            *v = remap[old];
        }
    }
    mesh.positions = positions;
}

fn stand(mesh: &mut MeshData, scale: f64) {
    let (lo, hi) = bounds(&mesh.positions);
    let origin = [(lo[0] + hi[0]) * 0.5, lo[1], (lo[2] + hi[2]) * 0.5];
    for p in &mut mesh.positions { *p = mul(sub(p.map(f64::from), origin), scale).map(|x| x as f32); }
}

fn validate_options(o: &Options) -> Result<(), String> {
    for (name, x) in [("nozzle", Some(o.nozzle_mm)), ("minimum feature", Some(o.min_feature_mm)), ("cell", o.cell_mm), ("height", o.height_mm)] {
        if x.is_some_and(|x| !x.is_finite() || x <= 0.0) { return Err(format!("{name} must be a positive finite number of millimetres")); }
        if x.is_some_and(|x| x > f32::MAX as f64 / 16.0) { return Err(format!("{name} is too large for mesh coordinates and their grid margin")); }
    }
    if o.max_triangles < 4 { return Err("a closed mesh needs at least four triangles".into()); }
    Ok(())
}

/// Repair without changing the input. The result is closed, edge-manifold,
/// centred in X/Z and standing on Y=0. Run this operation on a worker.
pub fn make_printable(mesh: &MeshData, o: &Options) -> Result<(MeshData, Report), String> {
    validate_options(o)?;
    let mut input = weld(mesh)?;
    let before = stats(&input);
    let overhang_before = overhang(&input, [0.0, 1.0, 0.0], o.nozzle_mm * 0.5);
    let rotated = if o.orient { orient(&mut input, o.nozzle_mm * 0.5) } else { None };
    let (lo, hi) = bounds(&input.positions);
    let scale = match o.height_mm {
        Some(height) if hi[1] > lo[1] => height / (hi[1] - lo[1]),
        Some(_) => return Err("the mesh has no height to scale".into()),
        None => 1.0,
    };
    stand(&mut input, scale);
    if input.positions.iter().flatten().any(|x| !x.is_finite()) { return Err("the requested size exceeds mesh coordinate precision".into()); }
    let mut h = choose_cell(&input, o);
    let mut retry_note: Option<String> = None;
    for attempt in 0..2 {
        let mut report = Report { before: before.clone(), rotated, scale, overhang_before_mm2: overhang_before * scale * scale, ..Report::default() };
        if let Some(note) = &retry_note { report.notes.push(note.clone()); }
        let mut result = solidify(&input, o, h, &mut report)?;
        if result.triangles.len() > o.max_triangles {
            if attempt == 1 { return Err(format!("the repaired mesh still has {} triangles after coarsening (budget {})", result.triangles.len(), o.max_triangles)); }
            // Allow for integer cell/triangle rounding on the second grid.
            h = report.cell_mm * (result.triangles.len() as f64 / o.max_triangles as f64).sqrt() * 1.08;
            retry_note = Some("coarsened the grid to fit the triangle budget".to_string());
            continue;
        }
        let (lo, hi) = bounds(&result.positions);
        if hi[1] <= lo[1] || result.positions.iter().flatten().any(|x| !x.is_finite()) { return Err("the repaired surface has no finite height".into()); }
        let finish_scale = o.height_mm.map_or(1.0, |height| height / (hi[1] - lo[1]));
        stand(&mut result, finish_scale);
        report.scale *= finish_scale;
        report.cell_mm *= finish_scale;
        report.closing_radius_mm *= finish_scale;
        report.base_cut_mm *= finish_scale;
        report.base_area_mm2 *= finish_scale.powi(2);
        report.thickened_mm3 *= finish_scale.powi(3);
        report.overhang_before_mm2 *= finish_scale.powi(2);
        report.after = stats(&result);
        let degenerate = result.triangles.len() - report.after.triangles;
        if report.after.open_edges != 0 || report.after.nonmanifold_edges != 0 || degenerate != 0 {
            return Err(format!("the repaired mesh has {} open edges, {} nonmanifold edges and {degenerate} degenerate triangles", report.after.open_edges, report.after.nonmanifold_edges));
        }
        report.overhang_after_mm2 = overhang(&result, [0.0, 1.0, 0.0], o.nozzle_mm * 0.5);
        if o.flat_base { report.base_area_mm2 = contact_area(&result); }
        if rotated.is_some() { report.notes.push("turned the model to reduce unsupported faces".into()); }
        if o.height_mm.is_some() { report.notes.push(format!("sized to {:.1} mm tall", report.after.size[1])); }
        if o.flat_base { report.notes.push(format!("flat base {:.1} square mm, {:.1} mm cut", report.base_area_mm2, report.base_cut_mm)); }
        return Ok((result, report));
    }
    unreachable!()
}

struct Face { points: [Point; 3], normal: Point, area: f64 }

fn faces(mesh: &MeshData) -> Vec<Face> {
    let (lo, hi) = bounds(&mesh.positions);
    let centre = mul(add(lo, hi), 0.5);
    let signed: f64 = mesh.triangles.iter().map(|&t| {
        let p = triangle(mesh, t).map(|p| sub(p, centre));
        dot(p[0], cross(p[1], p[2]))
    }).sum();
    let sign = if signed < 0.0 { -1.0 } else { 1.0 };
    mesh.triangles.iter().map(|&t| {
        let points = triangle(mesh, t);
        let n = normal(points);
        Face { points, normal: mul(unit(n), sign), area: length(n) * 0.5 }
    }).collect()
}

fn support_area(faces: &[Face], up: Point, bottom: f64, bed_tolerance: f64) -> f64 {
    faces.iter().filter(|f| {
        let vertical = dot(f.normal, up);
        // The absolute slope sets the 45-degree limit; only downward faces
        // count. A face resting on the bed needs no support.
        vertical < 0.0 && vertical.abs() > std::f64::consts::FRAC_1_SQRT_2
            && f.points.iter().any(|&p| dot(p, up) > bottom + bed_tolerance)
    }).map(|f| f.area).sum()
}

fn overhang(mesh: &MeshData, up: Point, tolerance: f64) -> f64 {
    let bottom = mesh.positions.iter().map(|p| dot(p.map(f64::from), up)).fold(f64::INFINITY, f64::min);
    support_area(&faces(mesh), up, bottom, tolerance)
}

fn rotation(up: Point) -> (Point, f64) {
    let angle = up[1].clamp(-1.0, 1.0).acos();
    let axis = cross(up, [0.0, 1.0, 0.0]);
    (if length(axis) < 1e-12 { [1.0, 0.0, 0.0] } else { unit(axis) }, angle)
}

fn rotate(p: Point, axis: Point, angle: f64) -> Point {
    let (s, c) = angle.sin_cos();
    add(add(mul(p, c), mul(cross(axis, p), s)), mul(axis, dot(axis, p) * (1.0 - c)))
}

fn pose_score(mesh: &MeshData, faces: &[Face], up: Point, tolerance: f64) -> (f64, f64) {
    let (axis, angle) = rotation(up);
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for p in &mesh.positions {
        let p = rotate(p.map(f64::from), axis, angle);
        for k in 0..3 { lo[k] = lo[k].min(p[k]); hi[k] = hi[k].max(p[k]); }
    }
    let size = sub(hi, lo);
    let area = support_area(faces, up, lo[1], tolerance);
    (area + 0.15 * size[1] * (size[0] * size[2]).sqrt(), area)
}

fn orient(mesh: &mut MeshData, tolerance: f64) -> Option<(Point, f64)> {
    let faces = faces(mesh);
    let mut candidates = vec![[0.0, 1.0, 0.0], [0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, -1.0]];
    let mut clusters: HashMap<[i32; 3], (f64, Point)> = HashMap::new();
    for face in &faces {
        let entry = clusters.entry(face.normal.map(|x| (x * 4.0).round() as i32)).or_default();
        entry.0 += face.area;
        entry.1 = add(entry.1, mul(face.normal, face.area));
    }
    let mut clusters: Vec<_> = clusters.into_iter().collect();
    clusters.sort_by(|a, b| b.1.0.total_cmp(&a.1.0).then(a.0.cmp(&b.0)));
    candidates.extend(clusters.iter().take(8).map(|(_, (_, n))| mul(unit(*n), -1.0)));
    let current = pose_score(mesh, &faces, candidates[0], tolerance);
    let (mut best, mut score) = (0, current);
    for (i, &up) in candidates.iter().enumerate().skip(1) {
        let candidate = pose_score(mesh, &faces, up, tolerance);
        if candidate.0 < score.0 { best = i; score = candidate; }
    }
    if best == 0 || score.1 >= current.1 * 0.75 { return None; }
    let (axis, angle) = rotation(candidates[best]);
    for p in &mut mesh.positions { *p = rotate(p.map(f64::from), axis, angle).map(|x| x as f32); }
    Some((axis, angle.to_degrees()))
}

/// Boundary walks are undirected, so flipped triangles do not break a loop.
/// A proper cup has a connected rim, not an open boundary to cap.
fn caps(mesh: &MeshData, radius: f64) -> (Vec<[[f32; 3]; 3]>, usize, usize) {
    let mut edges = edge_counts(mesh);
    let mut boundary: Vec<_> = edges.iter().filter(|(_, n)| **n == 1).map(|(&e, _)| e).collect();
    boundary.sort_unstable();
    let mut neighbours = vec![Vec::new(); mesh.positions.len()];
    for &(a, b) in &boundary { neighbours[a as usize].push(b); neighbours[b as usize].push(a); }
    let (mut caps, mut loops, mut capped) = (Vec::new(), 0, 0);
    for &(a, b) in &boundary {
        if edges[&(a, b)] != 1 { continue; }
        let (mut prev, mut at) = (a, b);
        let mut cycle = vec![a];
        edges.insert((a, b), 0);
        while at != a && neighbours[at as usize].len() == 2 {
            cycle.push(at);
            let ns = &neighbours[at as usize];
            let next = if ns[0] == prev { ns[1] } else { ns[0] };
            let edge = (at.min(next), at.max(next));
            if edges[&edge] != 1 { break; }
            edges.insert(edge, 0);
            prev = at;
            at = next;
        }
        if at != a || cycle.len() < 3 { continue; }
        loops += 1;
        let positions: Vec<_> = cycle.iter().map(|&v| mesh.positions[v as usize]).collect();
        let (lo, hi) = bounds(&positions);
        if length(sub(hi, lo)) <= 4.0 * radius { continue; }
        capped += 1;
        let centre = mul(positions.iter().fold([0.0; 3], |s, p| add(s, p.map(f64::from))), 1.0 / positions.len() as f64).map(|x| x as f32);
        for i in 0..positions.len() {
            let t = [centre, positions[i], positions[(i + 1) % positions.len()]];
            if length(normal(t.map(|p| p.map(f64::from)))) > 0.0 { caps.push(t); }
        }
    }
    (caps, loops, capped)
}

#[derive(Clone, Copy)]
struct Grid { origin: Point, n: [usize; 3], h: f64 }

impl Grid {
    fn new(mesh: &MeshData, h: f64, feature: f64) -> Self {
        let (lo, hi) = bounds(&mesh.positions);
        // Three empty cells outside even the widest blocking/thickening ball.
        let margin = (2.5 * h).max(0.6).max(feature * 0.5) + 3.0 * h;
        Self {
            origin: lo.map(|x| x - margin + h * 0.5),
            n: std::array::from_fn(|k| ((hi[k] - lo[k] + 2.0 * margin) / h).ceil() as usize + 1),
            h,
        }
    }
    fn len(self) -> usize { self.n.iter().product() }
    fn plane(self) -> usize { self.n[0] * self.n[1] }
    fn index(self, p: [usize; 3]) -> usize { (p[2] * self.n[1] + p[1]) * self.n[0] + p[0] }
    fn coords(self, i: usize) -> [usize; 3] { [i % self.n[0], i / self.n[0] % self.n[1], i / self.plane()] }
    fn point(self, p: [usize; 3]) -> Point { std::array::from_fn(|k| self.origin[k] + p[k] as f64 * self.h) }
    fn high(self) -> Point { self.point(self.n.map(|n| n - 1)) }
}

fn choose_cell(mesh: &MeshData, o: &Options) -> f64 {
    let area: f64 = mesh.triangles.iter().map(|&t| length(normal(triangle(mesh, t))) * 0.5).sum();
    o.cell_mm.unwrap_or_else(|| 0.25_f64.max((2.0 * area / o.max_triangles as f64).sqrt()))
}

fn bounded_grid(mesh: &MeshData, o: &Options, mut h: f64) -> Grid {
    let (lo, hi) = bounds(&mesh.positions);
    // Bound dimensions before converting them to usize, including explicit cells.
    h = h.max(length(sub(hi, lo)).max(o.min_feature_mm).max(0.6) / (makepad_csg_sdf::surface_mesh::MAX_CELLS - 20) as f64);
    loop {
        let grid = Grid::new(mesh, h, o.min_feature_mm);
        let count = grid.n.iter().map(|&n| n as f64).product::<f64>();
        let longest = *grid.n.iter().max().unwrap() as f64;
        // Leave room for the surface mesher's own two-cell border.
        let factor = (count / 16_000_000.0).cbrt().max(longest / (makepad_csg_sdf::surface_mesh::MAX_CELLS - 8) as f64);
        if factor <= 1.0 { return grid; }
        h *= factor * 1.01;
    }
}

/// Each worker owns whole rows/slabs; no atomics or overlapping writes.
fn parallel_mut<T: Send>(data: &mut [T], row: usize, f: impl Fn(usize, &mut [T]) + Sync) {
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get());
    let chunk = data.len().div_ceil(workers).div_ceil(row).max(1) * row;
    std::thread::scope(|scope| {
        for (i, part) in data.chunks_mut(chunk).enumerate() {
            let f = &f;
            scope.spawn(move || f(i * chunk, part));
        }
    });
}

struct RasterTriangle { points: [[f32; 3]; 3], lo: [usize; 3], hi: [usize; 3], normal: [f32; 3] }

fn raster_triangle(grid: Grid, points: [[f32; 3]; 3], radius: f64) -> RasterTriangle {
    let (lo, hi) = bounds(&points);
    RasterTriangle {
        points,
        lo: std::array::from_fn(|k| (((lo[k] - radius - grid.origin[k]) / grid.h).ceil().max(0.0) as usize).min(grid.n[k] - 1)),
        hi: std::array::from_fn(|k| (((hi[k] + radius - grid.origin[k]) / grid.h).floor().max(0.0) as usize).min(grid.n[k] - 1)),
        normal: unit(normal(points.map(|p| p.map(f64::from)))).map(|x| x as f32),
    }
}

fn raster_slab(grid: Grid, radius: f64, triangles: &[RasterTriangle], start: usize, slab: &mut [u8]) {
    let z0 = start / grid.plane();
    let z1 = z0 + slab.len() / grid.plane();
    let r2 = (radius * radius) as f32;
    for t in triangles {
        for z in t.lo[2].max(z0)..(t.hi[2] + 1).min(z1) {
            for y in t.lo[1]..=t.hi[1] {
                for x in t.lo[0]..=t.hi[0] {
                    let i = grid.index([x, y, z]) - start;
                    if slab[i] != 0 { continue; }
                    let p = grid.point([x, y, z]).map(|v| v as f32);
                    let plane: f32 = (0..3).map(|k| (p[k] - t.points[0][k]) * t.normal[k]).sum();
                    if plane * plane > r2 { continue; }
                    let q = closest_on_tri(p, t.points[0], t.points[1], t.points[2]);
                    let d2: f32 = (0..3).map(|k| (p[k] - q[k]).powi(2)).sum();
                    if d2 <= r2 { slab[i] = 1; }
                }
            }
        }
    }
}

fn band(grid: Grid, mesh: &MeshData, caps: Vec<[[f32; 3]; 3]>, radius: f64) -> Vec<u8> {
    let triangles: Vec<_> = mesh.triangles.iter().map(|t| t.map(|v| mesh.positions[v as usize])).chain(caps)
        .map(|p| raster_triangle(grid, p, radius)).collect();
    let mut blocked = vec![0; grid.len()];
    parallel_mut(&mut blocked, grid.plane(), |start, slab| raster_slab(grid, radius, &triangles, start, slab));
    blocked
}

fn flood(grid: Grid, blocked: &mut [u8]) {
    let mut queue = VecDeque::new();
    blocked[0] = 2;
    queue.push_back(0usize);
    while let Some(i) = queue.pop_front() {
        let p = grid.coords(i);
        for (axis, step) in [1, grid.n[0], grid.plane()].into_iter().enumerate() {
            for next in [if p[axis] > 0 { Some(i - step) } else { None }, if p[axis] + 1 < grid.n[axis] { Some(i + step) } else { None }].into_iter().flatten() {
                if blocked[next] == 0 { blocked[next] = 2; queue.push_back(next); }
            }
        }
    }
    for v in blocked { *v = u8::from(*v != 2); }
}

#[derive(Clone, Copy)]
struct Box3 { lo: Point, hi: Point }

impl Box3 {
    fn empty() -> Self { Self { lo: [f64::INFINITY; 3], hi: [f64::NEG_INFINITY; 3] } }
    fn include(&mut self, p: Point) { for k in 0..3 { self.lo[k] = self.lo[k].min(p[k]); self.hi[k] = self.hi[k].max(p[k]); } }
    fn overlaps(self, other: Self) -> bool { (0..3).all(|k| self.lo[k] <= other.hi[k] && other.lo[k] <= self.hi[k]) }
    fn volume(self) -> f64 { sub(self.hi, self.lo).iter().product() }
}

struct BoxTree { bounds: Box3, children: Option<(Box<BoxTree>, Box<BoxTree>)>, item: usize }

impl BoxTree {
    fn new(ids: &mut [usize], boxes: &[Box3]) -> Self {
        let mut bounds = Box3::empty();
        for &i in ids.iter() { bounds.include(boxes[i].lo); bounds.include(boxes[i].hi); }
        let mut out = Self { bounds, children: None, item: ids[0] };
        if ids.len() > 1 {
            let size = sub(bounds.hi, bounds.lo);
            let axis = (0..3).max_by(|&a, &b| size[a].total_cmp(&size[b])).unwrap();
            let mid = ids.len() / 2;
            ids.select_nth_unstable_by(mid, |&a, &b| (boxes[a].lo[axis] + boxes[a].hi[axis]).total_cmp(&(boxes[b].lo[axis] + boxes[b].hi[axis])).then(a.cmp(&b)));
            let (left, right) = ids.split_at_mut(mid);
            out.children = Some((Box::new(Self::new(left, boxes)), Box::new(Self::new(right, boxes))));
        }
        out
    }
    fn touches_other(&self, i: usize, bounds: Box3) -> bool {
        if !self.bounds.overlaps(bounds) { return false; }
        match &self.children {
            Some((left, right)) => left.touches_other(i, bounds) || right.touches_other(i, bounds),
            None => self.item != i,
        }
    }
}

/// A closing band can bridge a tiny floating piece one millimetre away. Remove
/// only provably detached input pieces: their bounding boxes neither touch
/// nor overlap any other piece, and even their box volume is below both limits.
/// The relative limit comes from the flooded solid, not its bounding box.
/// This does not classify any voxel or remove an intersecting pin/fin or skin.
fn remove_detached_dust(mesh: &mut MeshData, largest_solid_mm3: f64) -> usize {
    let mut sets = Sets::new(mesh.positions.len());
    for t in &mesh.triangles { sets.join(t[0] as usize, t[1] as usize); sets.join(t[0] as usize, t[2] as usize); }
    let mut ids = HashMap::new();
    let mut boxes = Vec::new();
    let mut pieces = Vec::with_capacity(mesh.triangles.len());
    for &t in &mesh.triangles {
        let root = sets.root(t[0] as usize);
        let i = *ids.entry(root).or_insert_with(|| { boxes.push(Box3::empty()); boxes.len() - 1 });
        for p in triangle(mesh, t) { boxes[i].include(p); }
        pieces.push(i);
    }
    if boxes.len() < 2 { return 0; }
    let tree = BoxTree::new(&mut (0..boxes.len()).collect::<Vec<_>>(), &boxes);
    let largest_box = boxes.iter().enumerate().max_by(|a, b| a.1.volume().total_cmp(&b.1.volume()).then(b.0.cmp(&a.0))).map(|(i, _)| i).unwrap();
    let remove: Vec<_> = boxes.iter().enumerate().map(|(i, &b)| i != largest_box && b.volume() < (0.01 * largest_solid_mm3).min(50.0) && !tree.touches_other(i, b)).collect();
    let count = remove.iter().filter(|&&r| r).count();
    let mut i = 0;
    mesh.triangles.retain(|_| { let keep = !remove[pieces[i]]; i += 1; keep });
    compact(mesh);
    count
}

struct Run { start: usize, len: usize }

fn join_runs(runs: &[Run], sets: &mut Sets, a: (usize, usize), b: (usize, usize), width: usize) {
    let (mut i, mut j) = (a.0, b.0);
    while i < a.1 && j < b.1 {
        let (x, y) = (runs[i].start % width, runs[j].start % width);
        let (end_x, end_y) = (x + runs[i].len, y + runs[j].len);
        // Ends are exclusive; equality still connects diagonal neighbours.
        if end_x < y { i += 1; }
        else if end_y < x { j += 1; }
        else { sets.join(i, j); if end_x < end_y { i += 1; } else { j += 1; } }
    }
}

/// Runs of occupied X cells reduce component work from 26 checks per voxel to
/// four merges per row, with the same 26-connected result.
fn components(grid: Grid, occupied: &[u8]) -> (Vec<Run>, Vec<usize>, Vec<usize>) {
    let mut runs = Vec::new();
    let mut sets = Sets::new(0);
    let mut rows = vec![(0, 0); grid.n[1] * grid.n[2]];
    for z in 0..grid.n[2] {
        for y in 0..grid.n[1] {
            let row = z * grid.n[1] + y;
            let begin = runs.len();
            let mut x = 0;
            while x < grid.n[0] {
                if occupied[row * grid.n[0] + x] == 0 { x += 1; continue; }
                let first = x;
                while x < grid.n[0] && occupied[row * grid.n[0] + x] != 0 { x += 1; }
                runs.push(Run { start: row * grid.n[0] + first, len: x - first });
                sets.push();
            }
            let current = (begin, runs.len());
            if y > 0 { join_runs(&runs, &mut sets, current, rows[row - 1], grid.n[0]); }
            if z > 0 {
                for py in y.saturating_sub(1)..=(y + 1).min(grid.n[1] - 1) {
                    join_runs(&runs, &mut sets, current, rows[(z - 1) * grid.n[1] + py], grid.n[0]);
                }
            }
            rows[row] = current;
        }
    }
    let roots: Vec<_> = (0..runs.len()).map(|i| sets.root(i)).collect();
    let mut sizes = vec![0; runs.len()];
    for (run, &root) in runs.iter().zip(&roots) { sizes[root] += run.len; }
    (runs, roots, sizes)
}

fn remove_islands(grid: Grid, solid: &mut [u8]) -> (usize, f64) {
    let (runs, roots, sizes) = components(grid, solid);
    let largest = sizes.iter().copied().max().unwrap_or(0);
    let threshold = (largest as f64 * 0.01).min(50.0 / grid.h.powi(3));
    let remove = |size: usize| size > 0 && (size as f64) < threshold;
    for (run, &root) in runs.iter().zip(&roots) {
        if remove(sizes[root]) { solid[run.start..run.start + run.len].fill(0); }
    }
    (sizes.into_iter().filter(|&size| remove(size)).count(), largest as f64 * grid.h.powi(3))
}

/// Lower envelope of parabolas: an exact squared Euclidean 1-D transform.
/// Infinite entries are skipped, so seed-free rows stay infinite without NaNs.
fn edt_line(input: &[f32], output: &mut [f32], sites: &mut Vec<usize>, cuts: &mut Vec<f64>) {
    sites.clear();
    cuts.clear();
    for (q, &value) in input.iter().enumerate() {
        if !value.is_finite() { continue; }
        let mut cut = f64::NEG_INFINITY;
        while let Some(&p) = sites.last() {
            cut = (value as f64 + (q * q) as f64 - input[p] as f64 - (p * p) as f64) / (2.0 * (q - p) as f64);
            if cut > *cuts.last().unwrap() { break; }
            sites.pop();
            cuts.pop();
        }
        if sites.is_empty() { cut = f64::NEG_INFINITY; }
        sites.push(q);
        cuts.push(cut);
    }
    if sites.is_empty() { output.fill(f32::INFINITY); return; }
    let mut k = 0;
    for (q, value) in output.iter_mut().enumerate() {
        while k + 1 < sites.len() && cuts[k + 1] < q as f64 { k += 1; }
        let d = q as f64 - sites[k] as f64;
        *value = (d * d + input[sites[k]] as f64) as f32;
    }
}

fn edt_rows(values: &mut [f32], width: usize) {
    parallel_mut(values, width, |_, slab| {
        let mut input = vec![0.0; width];
        let (mut sites, mut cuts) = (Vec::with_capacity(width), Vec::with_capacity(width));
        for row in slab.chunks_mut(width) {
            input.copy_from_slice(row);
            edt_line(&input, row, &mut sites, &mut cuts);
        }
    });
}

/// Rotate the axes so each following EDT pass has contiguous, independently
/// owned rows. Three rotations restore the original X/Y/Z order.
fn rotate_grid(input: &[f32], output: &mut [f32], n: [usize; 3]) {
    parallel_mut(output, n[1] * n[2], |start, slab| {
        for (i, value) in slab.iter_mut().enumerate() {
            let i = start + i;
            let (y, z, x) = (i % n[1], i / n[1] % n[2], i / (n[1] * n[2]));
            *value = input[(z * n[1] + y) * n[0] + x];
        }
    });
}

fn edt(grid: Grid, seeds: &[u8], seed: u8) -> Vec<f32> {
    let mut values = vec![0.0; seeds.len()];
    parallel_mut(&mut values, grid.plane(), |start, slab| {
        for (i, value) in slab.iter_mut().enumerate() { *value = if seeds[start + i] == seed { 0.0 } else { f32::INFINITY }; }
    });
    let mut scratch = vec![0.0; values.len()];
    let mut n = grid.n;
    for _ in 0..3 {
        edt_rows(&mut values, n[0]);
        rotate_grid(&values, &mut scratch, n);
        std::mem::swap(&mut values, &mut scratch);
        n = [n[1], n[2], n[0]];
    }
    values
}

fn signed_field(grid: Grid, solid: &[u8], outside_distance: &[f32], radius: f64) -> Vec<f32> {
    let mut field = edt(grid, solid, 1);
    parallel_mut(&mut field, grid.plane(), |start, slab| {
        for (i, value) in slab.iter_mut().enumerate() {
            let i = start + i;
            *value = if solid[i] != 0 { (radius - outside_distance[i].sqrt() as f64 * grid.h) as f32 }
                else { (*value as f64).sqrt().mul_add(grid.h, radius) as f32 };
        }
    });
    field
}

/// Separable [1,2,1] in each axis equals one 3x3x3 binomial pass.
fn blur(grid: Grid, field: &mut Vec<f32>) {
    let mut scratch = vec![0.0; field.len()];
    for (axis, step) in [1, grid.n[0], grid.plane()].into_iter().enumerate() {
        parallel_mut(&mut scratch, grid.plane(), |start, slab| {
            for (i, out) in slab.iter_mut().enumerate() {
                let i = start + i;
                let at = i / step % grid.n[axis];
                let a = if at == 0 { i } else { i - step };
                let b = if at + 1 == grid.n[axis] { i } else { i + step };
                *out = (field[a] + 2.0 * field[i] + field[b]) * 0.25;
            }
        });
        std::mem::swap(field, &mut scratch);
    }
}

fn local_maximum(grid: Grid, distances: &[f32], i: usize) -> bool {
    let [x, y, z] = grid.coords(i);
    for nz in z.saturating_sub(1)..=(z + 1).min(grid.n[2] - 1) {
        for ny in y.saturating_sub(1)..=(y + 1).min(grid.n[1] - 1) {
            for nx in x.saturating_sub(1)..=(x + 1).min(grid.n[0] - 1) {
                if distances[grid.index([nx, ny, nz])] > distances[i] { return false; }
            }
        }
    }
    true
}

fn medial_voxels(grid: Grid, distances: &[f32], radius: f64, feature_radius: f64) -> (Vec<usize>, usize) {
    let mut thin = vec![0; distances.len()];
    parallel_mut(&mut thin, grid.plane(), |start, slab| {
        for (i, v) in slab.iter_mut().enumerate() {
            // eO is measured in the dilated solid. Remove the closing radius
            // before testing the thickness of the original feature.
            *v = u8::from(distances[start + i].sqrt() as f64 * grid.h >= radius + feature_radius);
        }
    });
    let coverage = edt(grid, &thin, 1);
    parallel_mut(&mut thin, grid.plane(), |start, slab| {
        for (i, v) in slab.iter_mut().enumerate() {
            let i = start + i;
            let depth = distances[i].sqrt() as f64 * grid.h - radius;
            *v = u8::from(depth > 0.0 && coverage[i] as f64 * grid.h.powi(2) > feature_radius.powi(2));
        }
    });
    drop(coverage);
    let mut medial = vec![0; thin.len()];
    parallel_mut(&mut medial, grid.plane(), |start, slab| {
        for (i, v) in slab.iter_mut().enumerate() {
            let i = start + i;
            if thin[i] != 0 && distances[i].sqrt() as f64 * grid.h - radius >= 0.1 && local_maximum(grid, distances, i) { *v = 1; }
        }
    });
    let candidates: Vec<_> = medial.iter().enumerate().filter(|(_, v)| **v != 0).map(|(i, _)| i).collect();
    if candidates.is_empty() { return (candidates, 0); }
    let (runs, roots, sizes) = components(grid, &thin);
    let mut touched = vec![false; sizes.len()];
    for (run, &root) in runs.iter().zip(&roots) {
        if medial[run.start..run.start + run.len].contains(&1) { touched[root] = true; }
    }
    (candidates, touched.iter().filter(|&&x| x).count())
}

fn thicken_slab(grid: Grid, field: &mut [f32], start: usize, medial: &[[usize; 3]], radius: f64) {
    let reach = (radius / grid.h).ceil() as usize;
    let (z0, z1) = (start / grid.plane(), (start + field.len()) / grid.plane());
    for &[mx, my, mz] in medial {
        let low_z = mz.saturating_sub(reach).max(z0);
        let high_z = (mz + reach + 1).min(z1);
        if low_z >= high_z { continue; }
        for z in low_z..high_z {
            for y in my.saturating_sub(reach)..=(my + reach).min(grid.n[1] - 1) {
                for x in mx.saturating_sub(reach)..=(mx + reach).min(grid.n[0] - 1) {
                    let d2 = ((x as f64 - mx as f64).powi(2) + (y as f64 - my as f64).powi(2) + (z as f64 - mz as f64).powi(2)) * grid.h.powi(2);
                    if d2 > radius * radius { continue; }
                    let i = grid.index([x, y, z]) - start;
                    field[i] = field[i].min((d2.sqrt() - radius) as f32);
                }
            }
        }
    }
}

fn thicken(grid: Grid, field: &mut [f32], distances: &[f32], radius: f64, feature: f64, report: &mut Report) {
    let (medial, features) = medial_voxels(grid, distances, radius, feature * 0.5);
    if medial.is_empty() { return; }
    // Each voxel occurs once. Dense plateaux use a deterministic stride, as
    // opposed to admitting an unbounded number of overlapping balls.
    let stride = medial.len().div_ceil(200_000).max(1);
    if stride > 1 { report.notes.push(format!("sampled every {stride}th thin-feature centre on a dense medial plateau")); }
    let medial: Vec<_> = medial.into_iter().step_by(stride).map(|i| grid.coords(i)).collect();
    let before = field.iter().filter(|&&v| v < 0.0).count();
    parallel_mut(field, grid.plane(), |start, slab| thicken_slab(grid, slab, start, &medial, feature * 0.5));
    let after = field.iter().filter(|&&v| v < 0.0).count();
    report.thickened_mm3 = (after - before) as f64 * grid.h.powi(3);
    if after > before { report.notes.push(format!("thickened {features} thin features to {feature:.1} mm")); }
}

struct VoxelField { grid: Grid, values: Vec<f32>, base: Option<f64> }

impl Sdf3 for VoxelField {
    fn distance(&self, p: Vec3d) -> f64 {
        let q = sub([p.x, p.y, p.z], self.grid.origin).map(|x| x / self.grid.h);
        if (0..3).any(|k| q[k] < 0.0 || q[k] > (self.grid.n[k] - 1) as f64) { return 1e30; }
        let lo: [usize; 3] = std::array::from_fn(|k| (q[k].floor() as usize).min(self.grid.n[k] - 2));
        let f: Point = std::array::from_fn(|k| q[k] - lo[k] as f64);
        let mut value = 0.0;
        for z in 0..2 {
            for y in 0..2 {
                for x in 0..2 {
                    let weight = (if x == 0 { 1.0 - f[0] } else { f[0] })
                        * (if y == 0 { 1.0 - f[1] } else { f[1] }) * (if z == 0 { 1.0 - f[2] } else { f[2] });
                    value += weight * self.values[self.grid.index([lo[0] + x, lo[1] + y, lo[2] + z])] as f64;
                }
            }
        }
        // Intersect after interpolation too, so the base remains a true plane.
        self.base.map_or(value, |base| value.max(base - p.y))
    }
}

fn field_bounds(grid: Grid, field: &[f32]) -> Option<Box3> {
    let mut bounds = Box3::empty();
    for (i, &v) in field.iter().enumerate() {
        if v >= 0.0 { continue; }
        let at = grid.coords(i);
        let p = grid.point(at);
        bounds.include(p);
        for (axis, step) in [1, grid.n[0], grid.plane()].into_iter().enumerate() {
            for direction in [-1isize, 1] {
                if (direction < 0 && at[axis] == 0) || (direction > 0 && at[axis] + 1 == grid.n[axis]) { continue; }
                let next = (i as isize + direction * step as isize) as usize;
                if field[next] < 0.0 { continue; }
                let t = v as f64 / (v as f64 - field[next] as f64);
                let mut crossing = p;
                crossing[axis] += direction as f64 * grid.h * t;
                bounds.include(crossing);
            }
        }
    }
    if bounds.lo[0].is_finite() { Some(bounds) } else { None }
}

fn slice_area(grid: Grid, field: &[f32], y: f64) -> f64 {
    let at = (y - grid.origin[1]) / grid.h;
    if at < 0.0 || at >= (grid.n[1] - 1) as f64 { return 0.0; }
    let row = at.floor() as usize;
    let mix = at - row as f64;
    let mut count = 0;
    for z in 0..grid.n[2] {
        for x in 0..grid.n[0] {
            let i = grid.index([x, row, z]);
            if field[i] as f64 * (1.0 - mix) + field[i + grid.n[0]] as f64 * mix < 0.0 { count += 1; }
        }
    }
    count as f64 * grid.h.powi(2)
}

fn flat_base(grid: Grid, field: &mut [f32], report: &mut Report) -> Result<f64, String> {
    let bounds = field_bounds(grid, field).ok_or("the grid contains no printable solid")?;
    let size = sub(bounds.hi, bounds.lo);
    let target = 25.0_f64.max(0.015 * size[0] * size[2]);
    let limit = 0.04 * size[1];
    let steps = (limit / (grid.h * 0.25)).ceil().max(1.0) as usize;
    let mut depth = limit;
    for i in 0..=steps {
        let d = (i as f64 * grid.h * 0.25).min(limit);
        // Leave a half-cell allowance at the rim for polygonisation. An
        // already flat floor can pass at depth zero without being shaved.
        let probe = if i == 0 { bounds.lo[1] + grid.h * 0.001 } else { bounds.lo[1] + d - grid.h * 0.5 };
        if slice_area(grid, field, probe) >= target { depth = d; break; }
    }
    let base = bounds.lo[1] + depth;
    report.base_cut_mm = depth;
    report.base_area_mm2 = slice_area(grid, field, base + grid.h * 0.5);
    if slice_area(grid, field, base) < target { report.notes.push("the available base is smaller than the contact target within the 4% cut limit".into()); }
    parallel_mut(field, grid.plane(), |start, slab| {
        for (i, value) in slab.iter_mut().enumerate() {
            let y = grid.origin[1] + grid.coords(start + i)[1] as f64 * grid.h;
            *value = value.max((base - y) as f32);
        }
    });
    Ok(base)
}

/// Six Laplacian passes (three lambda/mu pairs) without changing topology.
/// Vertices on the contact plane are constrained so smoothing cannot round it.
fn smooth(mesh: &mut MeshData, base: Option<f64>, h: f64) {
    let mut offsets = vec![0usize; mesh.positions.len() + 1];
    for t in &mesh.triangles { for &v in t { offsets[v as usize + 1] += 2; } }
    for i in 1..offsets.len() { offsets[i] += offsets[i - 1]; }
    let mut cursor = offsets.clone();
    let mut neighbours = vec![0usize; *offsets.last().unwrap()];
    for t in &mesh.triangles {
        for k in 0..3 {
            let v = t[k] as usize;
            for n in [t[(k + 1) % 3], t[(k + 2) % 3]] { neighbours[cursor[v]] = n as usize; cursor[v] += 1; }
        }
    }
    let fixed: Vec<_> = mesh.positions.iter_mut().map(|p| {
        let fixed = base.is_some_and(|base| p[1] as f64 <= base + h * 0.005);
        if fixed { p[1] = base.unwrap() as f32; }
        fixed
    }).collect();
    let mut scratch = mesh.positions.clone();
    for _ in 0..3 {
        for weight in [0.5, -0.53] {
            parallel_mut(&mut scratch, 1, |start, slab| {
                for (i, p) in slab.iter_mut().enumerate() {
                    let i = start + i;
                    let ns = &neighbours[offsets[i]..offsets[i + 1]];
                    *p = mesh.positions[i];
                    if fixed[i] || ns.is_empty() { continue; }
                    let average = mul(ns.iter().fold([0.0; 3], |sum, &v| add(sum, mesh.positions[v].map(f64::from))), 1.0 / ns.len() as f64);
                    *p = add(p.map(f64::from), mul(sub(average, p.map(f64::from)), weight)).map(|v| v as f32);
                    if let Some(base) = base { p[1] = p[1].max(base as f32); }
                }
            });
            std::mem::swap(&mut mesh.positions, &mut scratch);
        }
    }
}

fn contact_area(mesh: &MeshData) -> f64 {
    let (lo, hi) = bounds(&mesh.positions);
    let tolerance = ((hi[1] - lo[1]) * 1e-6).max(1e-6);
    mesh.triangles.iter().map(|&t| triangle(mesh, t)).filter(|p| p.iter().all(|p| p[1] <= lo[1] + tolerance))
        .map(|p| normal(p)[1].abs() * 0.5).sum()
}

fn solidify(input: &MeshData, o: &Options, h: f64, report: &mut Report) -> Result<MeshData, String> {
    let grid = bounded_grid(input, o, h);
    let radius = (2.5 * grid.h).max(0.6);
    report.cell_mm = grid.h;
    report.closing_radius_mm = radius;
    if grid.h > h * 1.001 { report.notes.push("coarsened the grid to keep voxel storage bounded".into()); }
    if grid.h > o.min_feature_mm * 0.5 { report.notes.push("the triangle budget or cell size is too coarse to resolve every minimum-width detail".into()); }
    let (virtual_caps, mut loops, mut capped) = caps(input, radius);
    let mut solid = band(grid, input, virtual_caps, radius);
    flood(grid, &mut solid);
    let (removed, largest) = remove_islands(grid, &mut solid);
    let mut cleaned = input.clone();
    let dust = remove_detached_dust(&mut cleaned, largest);
    if dust > 0 {
        // Rebuild blocking without detached dust that dilation could have
        // joined to the body. Count only this final pass, avoiding double counts.
        let (virtual_caps, new_loops, new_capped) = caps(&cleaned, radius);
        loops = new_loops;
        capped = new_capped;
        solid = band(grid, &cleaned, virtual_caps, radius);
        flood(grid, &mut solid);
        report.islands_removed = dust + remove_islands(grid, &mut solid).0;
    } else {
        report.islands_removed = removed;
    }
    drop(cleaned);
    report.loops_capped = capped;
    let outside_distance = edt(grid, &solid, 0);
    let mut field = signed_field(grid, &solid, &outside_distance, radius);
    drop(solid);
    blur(grid, &mut field);
    thicken(grid, &mut field, &outside_distance, radius, o.min_feature_mm, report);
    drop(outside_distance);
    let base = if o.flat_base { Some(flat_base(grid, &mut field, report)?) } else { None };
    if !field.iter().any(|&v| v < 0.0) { return Err("no solid survives at this cell size; choose a smaller cell".into()); }
    let field = Arc::new(VoxelField { grid, values: field, base });
    // its values are distances (an exact transform's, blurred): each its own radius
    let mesh = mesh_surface(field, vec3(grid.origin), vec3(grid.high()), grid.h, Clearance::uniform(1.0));
    if mesh.triangles.is_empty() { return Err("the repaired field produced no surface".into()); }
    let mut mesh = MeshData { positions: mesh.vertices.iter().map(|v| [v.x as f32, v.y as f32, v.z as f32]).collect(), triangles: mesh.triangles };
    smooth(&mut mesh, base, grid.h);
    if loops > 0 { report.notes.push(format!("closed {loops} holes")); }
    if report.islands_removed > 0 { report.notes.push(format!("removed {} floating pieces", report.islands_removed)); }
    Ok(mesh)
}

#[cfg(test)]
#[path = "printable_tests.rs"]
mod tests;
