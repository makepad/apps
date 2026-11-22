//! Orientation estimates in the caller's frame. Run the search on a worker.

use makepad_fab_toolpath::par::{self, Ctl};
use std::f64::consts::PI;

/// A tube: its unit axis, in either direction, and how much it matters.
pub struct Tube {
    pub axis: [f64; 3],
    pub weight: f64,
}

/// The up that minimises the weighted squared sines of the tubes' bed angles.
/// Weights are nonnegative. No weight returns +Z; signs prefer +Z, then +Y, then +X.
pub fn flattest_up(tubes: &[Tube]) -> [f64; 3] {
    let scale = tubes.iter().map(|t| t.weight).fold(0.0, f64::max);
    if scale == 0.0 {
        return [0.0, 0.0, 1.0];
    }
    let mut m = [[0.0; 3]; 3];
    for t in tubes {
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] += (t.weight / scale) * t.axis[i] * t.axis[j];
            }
        }
    }
    let mut v = IDENTITY;
    loop {
        let (mut p, mut q) = (0, 1);
        for (i, j) in [(0, 2), (1, 2)] {
            if m[i][j].abs() > m[p][q].abs() {
                (p, q) = (i, j);
            }
        }
        let diagonal = (0..3).map(|i| m[i][i].abs()).fold(0.0, f64::max);
        if m[p][q].abs() <= f64::EPSILON * diagonal {
            break;
        }
        // Jacobi's smaller rotation removes the largest off-diagonal entry.
        let tau = (m[q][q] - m[p][p]) / (2.0 * m[p][q]);
        let t = tau.signum() / (tau.abs() + tau.hypot(1.0));
        let c = 1.0 / t.hypot(1.0);
        let s = t * c;
        let off = m[p][q];
        m[p][p] -= t * off;
        m[q][q] += t * off;
        m[p][q] = 0.0;
        m[q][p] = 0.0;
        for k in 0..3 {
            if k != p && k != q {
                let a = m[k][p];
                let b = m[k][q];
                m[k][p] = c * a - s * b;
                m[p][k] = m[k][p];
                m[k][q] = s * a + c * b;
                m[q][k] = m[k][q];
            }
            let a = v[k][p];
            let b = v[k][q];
            v[k][p] = c * a - s * b;
            v[k][q] = s * a + c * b;
        }
    }
    let mut least = 2;
    for i in [1, 0] {
        if m[i][i] < m[least][least] {
            least = i;
        }
    }
    let mut up = unit(std::array::from_fn(|i| v[i][least]));
    for i in [2, 1, 0] {
        if up[i] != 0.0 {
            if up[i] < 0.0 {
                up = mul(up, -1.0);
            }
            break;
        }
    }
    up
}

/// A unit axis's angle to the bed under unit `up`, in degrees, from flat to standing.
pub fn angle_to_bed(axis: [f64; 3], up: [f64; 3]) -> f64 {
    dot(axis, up).abs().clamp(0.0, 1.0).asin().to_degrees()
}

const IDENTITY: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// The smallest rotation, row-major, taking unit `up` to +Z.
/// At -Z the half turn is about X.
pub fn turn_to_up(up: [f64; 3]) -> [[f64; 3]; 3] {
    let s = up[0].hypot(up[1]);
    if s == 0.0 {
        return if up[2] >= 0.0 { IDENTITY } else {
            [[1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, -1.0]]
        };
    }
    let a = [up[1] / s, -up[0] / s, 0.0];
    let k = [[0.0, -a[2], a[1]], [a[2], 0.0, -a[0]], [-a[1], a[0], 0.0]];
    std::array::from_fn(|i| std::array::from_fn(|j| {
        up[2] * IDENTITY[i][j] + (1.0 - up[2]) * a[i] * a[j] + s * k[i][j]
    }))
}

/// A mesh in millimetres, with triangles counter-clockwise from outside.
pub struct OrientMesh<'a> {
    pub verts: &'a [[f64; 3]],
    pub tris: &'a [[u32; 3]],
}

/// One candidate up's estimate.
#[derive(Clone, Debug, PartialEq)]
pub struct UpScore {
    pub up: [f64; 3],
    /// Facet area past the angle, excluding the bed, in mm².
    pub overhang_mm2: f64,
    /// Support down to the bed or the part below, in mm³.
    pub support_mm3: f64,
    /// The part's height along `up`, in mm.
    pub height: f64,
    /// Facet area resting on the bed, in mm².
    pub bed_mm2: f64,
}

/// A unit tube axis that must stay within an angle of flat.
pub struct HeldTube {
    pub axis: [f64; 3],
    pub within_deg: f64,
}

// A nominal interface thickness gives overhang area a volume cost.
const INTERFACE_MM: f64 = 0.4;
const RAFT_THICKNESS: f64 = INTERFACE_MM;
// Less than a tenth of the footprint on the bed needs a raft, paying for what's missing.
const STANDS_ON: f64 = 0.1;
const BED_MM: f64 = 0.05;
const SAMPLES: usize = 1000;
const SAMPLE_FACETS: usize = 20_000;
const FACET_BATCH: usize = 128;
const GRID_CELLS: f64 = 128.0;
const FLAT_POSES: usize = 16;

struct Facet {
    normal: [f64; 3],
    area: f64,
}

struct Candidate {
    index: usize,
    score: UpScore,
    footprint: f64,
}

impl Candidate {
    fn cost(&self) -> f64 {
        let raft = RAFT_THICKNESS * (STANDS_ON * self.footprint - self.score.bed_mm2).max(0.0);
        self.score.support_mm3 + INTERFACE_MM * self.score.overhang_mm2 + raft
    }
}

fn rank(candidates: &mut [Candidate]) {
    candidates.sort_by(|a, b| a.cost().total_cmp(&b.cost()).then(a.index.cmp(&b.index)));
    let mut start = 0;
    while start < candidates.len() {
        let first = candidates[start].cost();
        let mut end = start + 1;
        while end < candidates.len() && candidates[end].cost() - first <= first * 0.01 {
            end += 1;
        }
        candidates[start..end].sort_by(|a, b| {
            b.score.bed_mm2.total_cmp(&a.score.bed_mm2)
                .then(a.score.height.total_cmp(&b.score.height))
                .then(a.index.cmp(&b.index))
        });
        start = end;
    }
}

/// The `top` best distinct ups for least support, best first; None on cancellation.
/// Flat faces join the best samples for full-mesh estimates; only samples get refined.
pub fn least_support_ups(
    mesh: &OrientMesh,
    limit_deg: f64,
    held: &[HeldTube],
    top: usize,
    stop: &(dyn Fn() -> bool + Sync),
) -> Option<Vec<UpScore>> {
    if stop() {
        return None;
    }
    if top == 0 || mesh.verts.is_empty() || mesh.tris.is_empty() {
        return Some(Vec::new());
    }
    let facets = facets(mesh, stop)?;
    let flat = flat_ups(&facets, stop)?;
    let ctl = Ctl { cancel: Some(stop), ..Ctl::default() };
    let stride = mesh.tris.len().div_ceil(SAMPLE_FACETS);
    let samples = par::try_map(ctl, SAMPLES, |index| {
        let up = sphere(index);
        if !holds(up, held) {
            return Some(None);
        }
        estimate(mesh, &facets, up, limit_deg, stride, false, stop)
            .map(|(score, footprint)| Some(Candidate { index: FLAT_POSES + index, score, footprint }))
    }).ok()?;
    let mut candidates: Vec<Candidate> = samples.into_iter().collect::<Option<Vec<_>>>()?
        .into_iter().flatten().collect();
    rank(&mut candidates);
    candidates.truncate(24);
    let mut poses: Vec<_> = flat.into_iter().enumerate().filter(|(_, up)| holds(*up, held)).collect();
    poses.extend(candidates.iter().map(|c| (c.index, c.score.up)));
    let better = par::try_map(ctl, poses.len(), |i| {
        let (index, up) = poses[i];
        estimate(mesh, &facets, up, limit_deg, 1, true, stop)
            .map(|(score, footprint)| Candidate { index, score, footprint })
    }).ok()?;
    let mut candidates = better.into_iter().collect::<Option<Vec<_>>>()?;
    rank(&mut candidates);
    let centres: Vec<_> = candidates.iter().filter(|c| c.index >= FLAT_POSES)
        .take(3).map(|c| c.score.up).collect();
    let refined = par::try_map(ctl, centres.len() * 16, |i| {
        let centre = centres[i / 16];
        let (x, y) = basis(centre);
        let theta = (if i % 16 < 8 { 4.0_f64 } else { 2.0_f64 }).to_radians();
        let phi = (i % 8) as f64 * PI / 4.0;
        let up = unit(add(mul(centre, theta.cos()), mul(add(mul(x, phi.cos()), mul(y, phi.sin())), theta.sin())));
        if !holds(up, held) {
            return Some(None);
        }
        estimate(mesh, &facets, up, limit_deg, 1, true, stop)
            .map(|(score, footprint)| Some(Candidate { index: FLAT_POSES + SAMPLES + i, score, footprint }))
    }).ok()?;
    candidates.extend(refined.into_iter().collect::<Option<Vec<_>>>()?.into_iter().flatten());
    rank(&mut candidates);
    let mut result: Vec<UpScore> = Vec::new();
    let close = 5.0_f64.to_radians().cos();
    for c in candidates {
        if result.iter().all(|s| dot(s.up, c.score.up) < close) {
            result.push(c.score);
            if result.len() == top {
                break;
            }
        }
    }
    if stop() { None } else { Some(result) }
}

/// One way up's estimate over the whole mesh, support by the grid; None on cancellation.
pub fn score_up(mesh: &OrientMesh, up: [f64; 3], limit_deg: f64, stop: &(dyn Fn() -> bool + Sync)) -> Option<UpScore> {
    if mesh.verts.is_empty() || mesh.tris.is_empty() {
        return Some(UpScore { up, overhang_mm2: 0.0, support_mm3: 0.0, height: 0.0, bed_mm2: 0.0 });
    }
    let facets = facets(mesh, stop)?;
    estimate(mesh, &facets, up, limit_deg, 1, true, stop).map(|(score, _)| score)
}

fn sphere(index: usize) -> [f64; 3] {
    let z = 1.0 - 2.0 * index as f64 / (SAMPLES - 1) as f64;
    let radius = (1.0 - z * z).max(0.0).sqrt();
    let phi = index as f64 * PI * (3.0 - 5.0_f64.sqrt());
    [radius * phi.cos(), radius * phi.sin(), z]
}

fn holds(up: [f64; 3], held: &[HeldTube]) -> bool {
    held.iter().all(|t| angle_to_bed(t.axis, up) <= t.within_deg)
}

fn flat_ups(facets: &[Facet], stop: &dyn Fn() -> bool) -> Option<Vec<[f64; 3]>> {
    struct Group {
        facet: usize,
        area: f64,
    }
    let mut order: Vec<_> = (0..facets.len()).filter(|&i| facets[i].area > 0.0).collect();
    order.sort_by(|&a, &b| facets[b].area.total_cmp(&facets[a].area).then(a.cmp(&b)));
    let close = 1.0_f64.to_radians().cos();
    let cell = 2.0 * 0.5_f64.to_radians().sin();
    let mut bins = std::collections::HashMap::<[i32; 3], Vec<usize>>::new();
    let mut groups: Vec<Group> = Vec::new();
    for (batch, i) in order.into_iter().enumerate() {
        if batch % FACET_BATCH == 0 && stop() {
            return None;
        }
        let f = &facets[i];
        let key = f.normal.map(|n| (n / cell).floor() as i32);
        let mut first = None;
        // Nearby normal bins contain every group within one degree.
        for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    if let Some(neighbours) = bins.get(&[key[0] + x, key[1] + y, key[2] + z]) {
                        for &g in neighbours {
                            if dot(f.normal, facets[groups[g].facet].normal) >= close {
                                first = Some(first.map_or(g, |old: usize| old.min(g)));
                            }
                        }
                    }
                }
            }
        }
        if let Some(g) = first {
            groups[g].area += f.area;
        } else {
            bins.entry(key).or_default().push(groups.len());
            groups.push(Group { facet: i, area: f.area });
        }
    }
    groups.sort_by(|a, b| b.area.total_cmp(&a.area).then(a.facet.cmp(&b.facet)));
    Some(groups.into_iter().take(FLAT_POSES).map(|g| mul(facets[g.facet].normal, -1.0)).collect())
}

fn facets(mesh: &OrientMesh, stop: &dyn Fn() -> bool) -> Option<Vec<Facet>> {
    let mut result = Vec::with_capacity(mesh.tris.len());
    for (i, tri) in mesh.tris.iter().enumerate() {
        if i % FACET_BATCH == 0 && stop() {
            return None;
        }
        let [a, b, c] = tri.map(|v| mesh.verts[v as usize]);
        let n = cross(sub(b, a), sub(c, a));
        let length = dot(n, n).sqrt();
        result.push(Facet { normal: if length == 0.0 { [0.0; 3] } else { mul(n, 1.0 / length) }, area: length * 0.5 });
    }
    Some(result)
}

fn estimate(
    mesh: &OrientMesh,
    facets: &[Facet],
    up: [f64; 3],
    limit: f64,
    stride: usize,
    grid: bool,
    stop: &dyn Fn() -> bool,
) -> Option<(UpScore, f64)> {
    let origin = mesh.verts[0];
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for (i, &p) in mesh.verts.iter().enumerate() {
        if i % FACET_BATCH == 0 && stop() {
            return None;
        }
        let h = dot(sub(p, origin), up);
        lo = lo.min(h);
        hi = hi.max(h);
    }
    let mut score = UpScore { up, overhang_mm2: 0.0, support_mm3: 0.0, height: hi - lo, bed_mm2: 0.0 };
    let mut footprint = 0.0;
    for (batch, i) in (0..facets.len()).step_by(stride).enumerate() {
        if batch % FACET_BATCH == 0 && stop() {
            return None;
        }
        let f = &facets[i];
        let h = mesh.tris[i].map(|v| dot(sub(mesh.verts[v as usize], origin), up) - lo);
        let area = f.area * stride as f64;
        let nz = dot(f.normal, up);
        footprint += area * (-nz).max(0.0);
        if h.iter().all(|&z| z <= BED_MM) {
            score.bed_mm2 += area;
        } else if past_limit(nz, limit) {
            score.overhang_mm2 += area;
            if !grid {
                score.support_mm3 += area * -nz * (h[0] + h[1] + h[2]) / 3.0;
            }
        }
    }
    if grid {
        score.support_mm3 = support_grid(mesh, facets, up, limit, origin, lo, stop)?;
    }
    Some((score, footprint))
}

fn past_limit(nz: f64, limit: f64) -> bool {
    nz < 0.0 && (-nz).clamp(-1.0, 1.0).asin().to_degrees() > limit
}

struct Crossing {
    height: f64,
    up: bool,
    support: bool,
    facet: usize,
}

fn support_grid(
    mesh: &OrientMesh,
    facets: &[Facet],
    up: [f64; 3],
    limit: f64,
    origin: [f64; 3],
    floor: f64,
    stop: &dyn Fn() -> bool,
) -> Option<f64> {
    let (x, y) = basis(up);
    let mut points = Vec::with_capacity(mesh.verts.len());
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for (i, &p) in mesh.verts.iter().enumerate() {
        if i % FACET_BATCH == 0 && stop() {
            return None;
        }
        let d = sub(p, origin);
        let p = [dot(d, x), dot(d, y), dot(d, up) - floor];
        for j in 0..2 {
            lo[j] = lo[j].min(p[j]);
            hi[j] = hi[j].max(p[j]);
        }
        points.push(p);
    }
    let cell = (hi[0] - lo[0]).max(hi[1] - lo[1]) / GRID_CELLS;
    if cell == 0.0 || hi[0] == lo[0] || hi[1] == lo[1] {
        return Some(0.0);
    }
    let width = ((hi[0] - lo[0]) / cell).ceil() as usize;
    let height = ((hi[1] - lo[1]) / cell).ceil() as usize;
    let mut cells: Vec<Vec<Crossing>> = (0..width * height).map(|_| Vec::new()).collect();
    for (i, tri) in mesh.tris.iter().enumerate() {
        if i % FACET_BATCH == 0 && stop() {
            return None;
        }
        let mut p = tri.map(|v| points[v as usize]);
        let nz = dot(facets[i].normal, up);
        let support = past_limit(nz, limit) && !p.iter().all(|p| p[2] <= BED_MM);
        let mut area = edge(p[0], p[1], p[2]);
        if area == 0.0 {
            continue;
        }
        if area < 0.0 {
            p.swap(1, 2);
            area = -area;
        }
        let bounds = |axis: usize, size: usize| {
            let min = p.iter().map(|p| p[axis]).fold(f64::INFINITY, f64::min);
            let max = p.iter().map(|p| p[axis]).fold(f64::NEG_INFINITY, f64::max);
            let first = ((min - lo[axis]) / cell - 0.5).ceil().max(0.0) as usize;
            let end = (((max - lo[axis]) / cell - 0.5).floor() + 1.0).max(0.0) as usize;
            (first.min(size), end.min(size))
        };
        let (x0, x1) = bounds(0, width);
        let (y0, y1) = bounds(1, height);
        for row in y0..y1 {
            for col in x0..x1 {
                let q = [lo[0] + (col as f64 + 0.5) * cell, lo[1] + (row as f64 + 0.5) * cell, 0.0];
                let e = [edge(p[1], p[2], q), edge(p[2], p[0], q), edge(p[0], p[1], q)];
                if (0..3).all(|j| e[j] > 0.0 || (e[j] == 0.0 && owns_edge(p[(j + 1) % 3], p[(j + 2) % 3]))) {
                    let h = (e[0] * p[0][2] + e[1] * p[1][2] + e[2] * p[2][2]) / area;
                    cells[row * width + col].push(Crossing { height: h, up: nz > 0.0, support, facet: i });
                }
            }
        }
    }
    let mut volume = 0.0;
    for (i, crossings) in cells.iter_mut().enumerate() {
        if i % FACET_BATCH == 0 && stop() {
            return None;
        }
        crossings.sort_by(|a, b| a.height.total_cmp(&b.height)
            .then(b.up.cmp(&a.up)).then(a.facet.cmp(&b.facet)));
        let mut base = 0.0;
        for c in crossings {
            if c.up {
                base = c.height;
            } else if c.support {
                volume += (c.height - base).max(0.0) * cell * cell;
            }
        }
    }
    Some(volume)
}

fn edge(a: [f64; 3], b: [f64; 3], p: [f64; 3]) -> f64 {
    (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
}

// Half-open edges give a shared edge's ray to just one triangle.
fn owns_edge(a: [f64; 3], b: [f64; 3]) -> bool {
    b[1] > a[1] || (b[1] == a[1] && b[0] < a[0])
}

fn basis(up: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let mut least = 0;
    for i in 1..3 {
        if up[i].abs() < up[least].abs() {
            least = i;
        }
    }
    let x = unit(cross(IDENTITY[least], up));
    (x, cross(up, x))
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] { std::array::from_fn(|i| a[i] + b[i]) }
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] { std::array::from_fn(|i| a[i] - b[i]) }
fn mul(a: [f64; 3], s: f64) -> [f64; 3] { a.map(|v| v * s) }
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 { (0..3).map(|i| a[i] * b[i]).sum() }
fn unit(a: [f64; 3]) -> [f64; 3] { mul(a, 1.0 / dot(a, a).sqrt()) }
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Mesh {
        verts: Vec<[f64; 3]>,
        tris: Vec<[u32; 3]>,
    }

    impl Mesh {
        fn view(&self) -> OrientMesh<'_> {
            OrientMesh { verts: &self.verts, tris: &self.tris }
        }

        fn add_box(&mut self, lo: [f64; 3], hi: [f64; 3]) {
            let start = self.verts.len() as u32;
            for p in [[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
                      [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]] {
                self.verts.push(std::array::from_fn(|i| if p[i] == 0 { lo[i] } else { hi[i] }));
            }
            for t in [[0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7],
                      [0, 1, 5], [0, 5, 4], [1, 2, 6], [1, 6, 5],
                      [2, 3, 7], [2, 7, 6], [3, 0, 4], [3, 4, 7]] {
                self.tris.push(t.map(|v| v + start));
            }
        }

        fn cylinder(axis: [f64; 3], radius: f64, length: f64) -> Self {
            let mut mesh = Self::default();
            let (x, y) = basis(axis);
            let sides = 24_u32;
            for h in [0.0, length] {
                for i in 0..sides {
                    let a = i as f64 * 2.0 * PI / sides as f64;
                    mesh.verts.push(add(mul(axis, h), mul(add(mul(x, a.cos()), mul(y, a.sin())), radius)));
                }
            }
            mesh.verts.extend([[0.0; 3], mul(axis, length)]);
            for i in 0..sides {
                let j = (i + 1) % sides;
                mesh.tris.extend([[i, j, j + sides], [i, j + sides, i + sides],
                    [2 * sides, j, i], [2 * sides + 1, i + sides, j + sides]]);
            }
            mesh
        }

        fn dome(radius: f64) -> Self {
            let mut mesh = Self::default();
            let (sides, rings) = (32_u32, 8_u32);
            for r in 0..rings {
                let latitude = r as f64 * PI / (2 * rings) as f64;
                for i in 0..sides {
                    let longitude = i as f64 * 2.0 * PI / sides as f64;
                    mesh.verts.push([radius * latitude.cos() * longitude.cos(),
                        radius * latitude.cos() * longitude.sin(), radius * latitude.sin()]);
                }
            }
            let crown = mesh.verts.len() as u32;
            mesh.verts.extend([[0.0, 0.0, radius], [0.0; 3]]);
            for i in 0..sides {
                let j = (i + 1) % sides;
                mesh.tris.push([crown + 1, j, i]);
                mesh.tris.push([(rings - 1) * sides + i, (rings - 1) * sides + j, crown]);
                for r in 0..rings - 1 {
                    let a = r * sides;
                    mesh.tris.extend([[a + i, a + j, a + sides + j], [a + i, a + sides + j, a + sides + i]]);
                }
            }
            mesh
        }

        fn table() -> Self {
            let mut mesh = Self::default();
            mesh.add_box([-15.0, -15.0, 20.0], [15.0, 15.0, 22.0]);
            for x in [-13.0, 9.0] {
                for y in [-13.0, 9.0] {
                    mesh.add_box([x, y, 0.0], [x + 4.0, y + 4.0, 20.0]);
                }
            }
            mesh
        }

        fn score(&self, up: [f64; 3]) -> UpScore {
            let mesh = self.view();
            estimate(&mesh, &facets(&mesh, &|| false).unwrap(), up, 45.0, 1, true, &|| false).unwrap().0
        }

        fn search(&self, held: &[HeldTube]) -> Vec<UpScore> {
            least_support_ups(&self.view(), 45.0, held, 6, &|| false).unwrap()
        }
    }

    #[test]
    fn orient_strength_and_angles() {
        let (x, y, z) = (IDENTITY[0], IDENTITY[1], IDENTITY[2]);
        let tube = |axis, weight| Tube { axis, weight };
        assert_eq!(flattest_up(&[]), z);
        assert_eq!(flattest_up(&[tube(x, 0.0)]), z);
        assert!(angle_to_bed(z, flattest_up(&[tube(z, 1.0)])) < 1e-6);
        let up = flattest_up(&[tube(x, 1.0), tube(y, 1.0)]);
        assert_eq!(up, z);
        assert_eq!(angle_to_bed(x, up), 0.0);
        assert_eq!(angle_to_bed(y, up), 0.0);
        for (weights, expected) in [([3.0, 2.0, 1.0], z), ([1.0, 2.0, 3.0], x)] {
            let up = flattest_up(&[tube(x, weights[0]), tube(y, weights[1]), tube(z, weights[2])]);
            assert_eq!(up, expected);
            for axis in [x, y, z] {
                assert_eq!(angle_to_bed(axis, up), if axis == expected { 90.0 } else { 0.0 });
            }
        }
        let diagonal = unit([1.0, 2.0, 3.0]);
        let up = flattest_up(&[tube(diagonal, 7.0)]);
        assert!(angle_to_bed(diagonal, up) < 1e-6, "diagonal tube must lie flat: {up:?}");
        assert!(up[2] >= 0.0, "sign prefers +Z");
        // A rotated weighted triad exercises all Jacobi pivots.
        let (a, b) = basis(diagonal);
        let up = flattest_up(&[tube(a, 3.0), tube(b, 2.0), tube(diagonal, 1.0)]);
        assert!(dot(up, diagonal) > 1.0 - 1e-12);
        for axis in [x, mul(x, -1.0)] {
            assert_eq!(angle_to_bed(axis, z), 0.0);
            assert_eq!(angle_to_bed(axis, x), 90.0);
            assert!((angle_to_bed(axis, unit([1.0, 0.0, 1.0])) - 45.0).abs() < 1e-12);
        }
    }

    #[test]
    fn orient_turn_is_the_smallest_rotation() {
        for up in [IDENTITY[2], [0.0, 0.0, -1.0], unit([1.0, -2.0, 3.0]), unit([1e-12, 0.0, -1.0])] {
            let r = turn_to_up(up);
            let mapped = r.map(|row| dot(row, up));
            for i in 0..3 {
                assert!((mapped[i] - IDENTITY[2][i]).abs() < 1e-9, "up maps to +Z");
                for j in 0..3 {
                    assert!((dot(r[i], r[j]) - IDENTITY[i][j]).abs() < 1e-9, "orthonormal rows");
                }
            }
            assert!((dot(r[0], cross(r[1], r[2])) - 1.0).abs() < 1e-9, "determinant is one");
            assert!((r[0][0] + r[1][1] + r[2][2] - 1.0 - 2.0 * up[2]).abs() < 1e-9, "smallest angle");
        }
        assert_eq!(turn_to_up(IDENTITY[2]), IDENTITY);
        assert_eq!(turn_to_up([0.0, 0.0, -1.0]), [[1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, -1.0]]);
    }

    #[test]
    fn orient_table_and_dome_find_their_bases() {
        let table = Mesh::table();
        let choices = table.search(&[]);
        let best = &choices[0];
        let standing = table.score(IDENTITY[2]);
        assert!(best.up[2] < -5.0_f64.to_radians().cos(), "table on its top: {best:?}");
        assert!((best.bed_mm2 - 900.0).abs() < 1e-6, "top slab rests on bed");
        assert!(best.support_mm3 < 1e-6, "inverted table needs no support: {best:?}");
        assert!(standing.support_mm3 > 10_000.0, "standing table needs support: {standing:?}");

        let dome = Mesh::dome(10.0);
        let choices = dome.search(&[]);
        let best = &choices[0];
        assert!(best.up[2] > 5.0_f64.to_radians().cos(), "dome on its base: {best:?}");
        assert!(best.support_mm3 < 1e-6, "upright dome needs no support: {best:?}");
        assert!(best.bed_mm2 > 300.0, "flat base rests on bed");
        for (i, a) in choices.iter().enumerate() {
            for b in &choices[..i] {
                assert!(dot(a.up, b.up) < 5.0_f64.to_radians().cos(), "choices are distinct");
            }
        }
    }

    #[test]
    fn orient_held_beam_and_cylinder() {
        let mut beam = Mesh::default();
        beam.add_box([0.0; 3], [60.0, 4.0, 4.0]);
        let held = [HeldTube { axis: IDENTITY[0], within_deg: 10.0 }];
        let choices = beam.search(&held);
        assert!(!choices.is_empty());
        for s in &choices {
            assert!(angle_to_bed(held[0].axis, s.up) <= 10.0, "beam held flat");
        }
        assert!(choices[0].height < 15.0, "best beam is not standing");
        let axis = unit([1.0, 2.0, 3.0]);
        let cylinder = Mesh::cylinder(axis, 2.0, 20.0);
        let held = [HeldTube { axis, within_deg: 10.0 }];
        let choices = cylinder.search(&held);
        assert!(!choices.is_empty());
        assert!(choices.iter().all(|s| angle_to_bed(axis, s.up) <= 10.0));
        let standing = cylinder.score(axis);
        assert!((standing.height - 20.0).abs() < 1e-9);
        assert!(standing.support_mm3 < 1e-9, "cylinder caps face outward");
        assert!(standing.bed_mm2 > 12.0);
    }

    #[test]
    fn orient_bracket_rests_on_its_big_face() {
        let mut bracket = Mesh::default();
        for x in [0.0, 30.0] {
            for [y, z] in [[0.0, 0.0], [20.0, 0.0], [20.0, 10.0], [18.0, 10.0], [18.0, 2.0], [0.0, 2.0]] {
                bracket.verts.push([x, y, z]);
            }
        }
        for [a, b, c] in [[0, 1, 4], [0, 4, 5], [1, 2, 3], [1, 3, 4]] {
            bracket.tris.extend([[a, c, b], [a + 6, b + 6, c + 6]]);
        }
        for i in 0..6 {
            let j = (i + 1) % 6;
            bracket.tris.extend([[i, j, j + 6], [i, j + 6, i + 6]]);
        }
        let standing = bracket.score(IDENTITY[1]);
        assert!(standing.overhang_mm2 > 200.0, "upright bracket has an underside overhang");
        // A flat face away from the sample poles must reach the full-mesh estimate.
        let rotation = turn_to_up(unit([1.0, 2.0, 3.0]));
        let base_up = rotation.map(|row| dot(row, IDENTITY[2]));
        for p in &mut bracket.verts {
            *p = rotation.map(|row| dot(row, *p));
        }
        let choices = bracket.search(&[]);
        let best = &choices[0];
        assert!(dot(best.up, base_up) > 5.0_f64.to_radians().cos(), "bracket on its big face: {best:?}");
        assert!((best.bed_mm2 - 600.0).abs() < 1e-6, "big face rests on bed");
        assert!(best.support_mm3 < 1e-6, "flat bracket needs no support: {best:?}");
    }

    #[test]
    fn orient_shelf_support_stops_at_the_part() {
        let mut shelf = Mesh::default();
        shelf.add_box([-1.0, -1.0, 0.0], [1.0, 1.0, 8.0]);
        shelf.add_box([-4.0, -4.0, 8.0], [4.0, 4.0, 10.0]);
        let bare = shelf.score(IDENTITY[2]);
        shelf.add_box([-5.0, -5.0, 0.0], [5.0, 5.0, 4.0]);
        let base = shelf.score(IDENTITY[2]);
        assert!((bare.support_mm3 - 480.0).abs() < 5.0, "rays count the shelf once: {bare:?}");
        assert!((base.support_mm3 - 240.0).abs() < 5.0, "support stops on the wider base: {base:?}");
        assert!(base.support_mm3 < bare.support_mm3 * 0.55);
        assert_eq!(bare.overhang_mm2, base.overhang_mm2);
    }

    #[test]
    fn orient_repeatable_across_thread_counts() {
        struct Restore(usize);
        impl Drop for Restore {
            fn drop(&mut self) { par::set_threads(self.0); }
        }
        let _restore = Restore(par::threads());
        let mesh = Mesh::dome(10.0);
        par::set_threads(3);
        let start = std::time::Instant::now();
        let first = mesh.search(&[]);
        eprintln!("orient largest mesh: {} triangles, least_support_ups: {:?}, 3 threads", mesh.tris.len(), start.elapsed());
        let second = mesh.search(&[]);
        par::set_threads(1);
        let single = mesh.search(&[]);
        assert_eq!(first, second, "repeat runs match every score");
        assert_eq!(first, single, "single-thread scores match parallel scores");
    }

    #[test]
    fn orient_cancel_and_empty() {
        let mesh = Mesh::table();
        assert!(least_support_ups(&mesh.view(), 45.0, &[], 3, &|| true).is_none());
        let checks = std::sync::atomic::AtomicUsize::new(0);
        let stop = || checks.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 10;
        assert!(least_support_ups(&mesh.view(), 45.0, &[], 3, &stop).is_none(), "cancellation during scoring");
        assert_eq!(least_support_ups(&mesh.view(), 45.0, &[], 0, &|| false), Some(Vec::new()));
        assert_eq!(Mesh::default().search(&[]), Vec::new());
    }

    #[test]
    fn orient_rank_uses_groups_from_the_first_score() {
        let candidate = |index, cost, bed, height| Candidate { index, footprint: 0.0, score: UpScore {
            up: IDENTITY[2], support_mm3: cost, overhang_mm2: 0.0, bed_mm2: bed, height,
        }};
        let mut list = vec![candidate(4, 101.8, 100.0, 1.0), candidate(3, 100.9, 4.0, 2.0),
            candidate(2, 100.5, 4.0, 2.0), candidate(1, 100.2, 4.0, 3.0), candidate(0, 100.0, 2.0, 1.0)];
        rank(&mut list);
        assert_eq!(list.iter().map(|c| c.index).collect::<Vec<_>>(), [2, 3, 1, 0, 4]);
    }
}
