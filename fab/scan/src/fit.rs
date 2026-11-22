//! What a scan reconstructed against the object's model: the table found, the object's points cut
//! from the room's, the model fitted to them, the distances. Nothing here keeps the room.
//!
//! World: tracking space, y against gravity, metres. The model is read as Fab exports a plate:
//! binary STL, millimetres, z up, standing on its plate; [`Mesh`] holds it with y up, in metres.

type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn times(a: V3, s: f64) -> V3 {
    a.map(|v| v * s)
}

fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn length(a: V3) -> f64 {
    dot(a, a).sqrt()
}

/// The nearest point of triangle `t` to `p` (Ericson, Real-Time Collision Detection, 5.1.5).
fn closest_on_triangle(p: V3, t: &[V3; 3]) -> V3 {
    let [a, b, c] = *t;
    let (ab, ac, ap) = (sub(b, a), sub(c, a), sub(p, a));
    let (d1, d2) = (dot(ab, ap), dot(ac, ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = sub(p, b);
    let (d3, d4) = (dot(ab, bp), dot(ac, bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return add(a, times(ab, d1 / (d1 - d3)));
    }
    let cp = sub(p, c);
    let (d5, d6) = (dot(ab, cp), dot(ac, cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return add(a, times(ac, d2 / (d2 - d6)));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && d4 - d3 >= 0.0 && d5 - d6 >= 0.0 {
        return add(b, times(sub(c, b), (d4 - d3) / ((d4 - d3) + (d5 - d6))));
    }
    let denominator = 1.0 / (va + vb + vc);
    add(a, add(times(ab, vb * denominator), times(ac, vc * denominator)))
}

/// The model's triangles on a uniform grid.
pub struct Mesh {
    triangles: Vec<[V3; 3]>,
    normals: Vec<V3>,
    pub low: V3,
    pub high: V3,
    cell: f64,
    dims: [usize; 3],
    cells: Vec<Vec<u32>>,
}

/// A nearest point on the model.
#[derive(Clone, Copy)]
pub struct Hit {
    pub point: V3,
    pub normal: V3,
    pub distance: f64,
}

impl Mesh {
    pub fn from_stl(bytes: &[u8]) -> Result<Mesh, String> {
        let count = bytes.get(80..84).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize).ok_or("not a binary STL")?;
        if bytes.len() != count.saturating_mul(50).saturating_add(84) {
            return Err("not a binary STL: its size does not match its triangle count".into());
        }
        let corner = |record: &[u8], at: usize| -> V3 {
            let value = |i: usize| f64::from(f32::from_le_bytes(record[at + 4 * i..at + 4 * i + 4].try_into().unwrap())) / 1000.0;
            // x, z up, -y: the world's y is the model's height
            [value(0), value(2), -value(1)]
        };
        let triangles: Vec<[V3; 3]> = bytes[84..].chunks_exact(50).map(|r| [corner(r, 12), corner(r, 24), corner(r, 36)]).collect();
        Mesh::new(triangles)
    }

    pub fn new(mut triangles: Vec<[V3; 3]>) -> Result<Mesh, String> {
        let area = |t: &[V3; 3]| length(cross(sub(t[1], t[0]), sub(t[2], t[0])));
        triangles.retain(|t| t.iter().flatten().all(|v| v.is_finite()) && area(t) > 1e-14);
        if triangles.is_empty() {
            return Err("a model without triangles".into());
        }
        let normals: Vec<V3> = triangles.iter().map(|t| { let n = cross(sub(t[1], t[0]), sub(t[2], t[0])); times(n, 1.0 / length(n)) }).collect();
        let (mut low, mut high) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for v in triangles.iter().flatten() {
            for i in 0..3 {
                low[i] = low[i].min(v[i]);
                high[i] = high[i].max(v[i]);
            }
        }
        let extent = sub(high, low);
        let cell = (extent[0].max(extent[1]).max(extent[2]) / 48.0).max(0.002);
        let dims = extent.map(|e| ((e / cell).ceil() as usize).max(1));
        let mut cells = vec![Vec::new(); dims[0] * dims[1] * dims[2]];
        let at = |v: f64, axis: usize| (((v - low[axis]) / cell) as usize).min(dims[axis] - 1);
        for (index, t) in triangles.iter().enumerate() {
            let range = |axis: usize| {
                let values = t.map(|v| v[axis]);
                at(values[0].min(values[1]).min(values[2]), axis)..=at(values[0].max(values[1]).max(values[2]), axis)
            };
            for x in range(0) {
                for y in range(1) {
                    for z in range(2) {
                        cells[(z * dims[1] + y) * dims[0] + x].push(index as u32);
                    }
                }
            }
        }
        Ok(Mesh { triangles, normals, low, high, cell, dims, cells })
    }

    /// The model's nearest point to `p`, if within `limit`.
    pub fn closest(&self, p: V3, limit: f64) -> Option<Hit> {
        let start: [usize; 3] = std::array::from_fn(|i| ((p[i] - self.low[i]) / self.cell).clamp(0.0, (self.dims[i] - 1) as f64) as usize);
        let mut best: Option<Hit> = None;
        let mut best_distance = limit;
        for ring in 0..=self.dims[0].max(self.dims[1]).max(self.dims[2]) {
            // what the rings ahead hold is at least this far
            let ahead = ring.saturating_sub(1) as f64 * self.cell;
            if ahead >= best_distance {
                break;
            }
            let span = |i: usize| start[i].saturating_sub(ring)..=(start[i] + ring).min(self.dims[i] - 1);
            for z in span(2) {
                for y in span(1) {
                    for x in span(0) {
                        if x.abs_diff(start[0]).max(y.abs_diff(start[1])).max(z.abs_diff(start[2])) != ring {
                            continue;
                        }
                        for &index in &self.cells[(z * self.dims[1] + y) * self.dims[0] + x] {
                            let point = closest_on_triangle(p, &self.triangles[index as usize]);
                            let distance = length(sub(p, point));
                            if distance < best_distance {
                                best_distance = distance;
                                best = Some(Hit { point, normal: self.normals[index as usize], distance });
                            }
                        }
                    }
                }
            }
        }
        best
    }

    /// `count` points evenly over the model's surface, leaving out faces looking down (it stands on
    /// those), with a fixed seed.
    fn surface_samples(&self, count: usize) -> Vec<V3> {
        let faces: Vec<usize> = (0..self.triangles.len()).filter(|&i| self.normals[i][1] > -0.5).collect();
        let area = |i: usize| { let t = &self.triangles[i]; 0.5 * length(cross(sub(t[1], t[0]), sub(t[2], t[0]))) };
        let total: f64 = faces.iter().map(|&i| area(i)).sum();
        let mut random = Random(5);
        let mut out = Vec::with_capacity(count);
        for &i in &faces {
            let t = &self.triangles[i];
            let share = area(i) / total * count as f64;
            let mut n = share.floor() as usize;
            if random.unit() < share - share.floor() {
                n += 1;
            }
            for _ in 0..n {
                let (mut u, mut v) = (random.unit(), random.unit());
                if u + v > 1.0 {
                    (u, v) = (1.0 - u, 1.0 - v);
                }
                out.push(add(t[0], add(times(sub(t[1], t[0]), u), times(sub(t[2], t[0]), v))));
            }
        }
        out
    }
}

/// SplitMix64.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// `f` of each point on all cores, in order.
fn map_points<T: Send>(points: &[V3], f: impl Fn(V3) -> T + Sync) -> Vec<T> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let chunk = points.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let parts: Vec<_> = points
            .chunks(chunk)
            .map(|part| {
                let f = &f;
                scope.spawn(move || part.iter().map(|&p| f(p)).collect::<Vec<T>>())
            })
            .collect();
        parts.into_iter().flat_map(|part| part.join().expect("a point thread")).collect()
    })
}

/// The table: most points on one level plane near `around`.
pub struct Table {
    /// Up, unit, y positive.
    pub normal: V3,
    /// `dot(normal, p)` on the plane.
    pub height: f64,
    pub points: usize,
}

const TABLE_REACH: f64 = 0.4;
const TABLE_TOLERANCE: f64 = 0.003;
const TABLE_TRIES: usize = 2000;

pub fn find_table(points: &[V3], around: V3) -> Result<Table, String> {
    let near: Vec<V3> = points.iter().copied().filter(|&p| length(sub(p, around)) < TABLE_REACH).collect();
    if near.len() < 1000 {
        return Err(format!("{} points near what was looked at", near.len()));
    }
    let stride = (near.len() / 20_000).max(1);
    let sample: Vec<V3> = near.iter().copied().step_by(stride).collect();
    let mut random = Random(9);
    let mut best: Option<(usize, V3, f64)> = None;
    for _ in 0..TABLE_TRIES {
        let [a, b, c] = [0, 1, 2].map(|_| sample[random.below(sample.len())]);
        let n = cross(sub(b, a), sub(c, a));
        let l = length(n);
        if l < 1e-9 || (n[1] / l).abs() < 0.95 {
            continue;
        }
        let n = times(n, n[1].signum() / l);
        let height = dot(n, a);
        let count = sample.iter().filter(|&&p| (dot(n, p) - height).abs() < TABLE_TOLERANCE).count();
        if best.as_ref().is_none_or(|b| count > b.0) {
            best = Some((count, n, height));
        }
    }
    let (_, mut normal, mut height) = best.ok_or("no level plane")?;
    // y = a x + b z + c over the plane's points, twice
    let mut count = 0;
    for _ in 0..2 {
        let mut m = [[0.0; 4]; 3];
        count = 0;
        for &p in &near {
            if (dot(normal, p) - height).abs() >= TABLE_TOLERANCE {
                continue;
            }
            count += 1;
            let row = [p[0] - around[0], p[2] - around[2], 1.0];
            for i in 0..3 {
                for j in 0..3 {
                    m[i][j] += row[i] * row[j];
                }
                m[i][3] += row[i] * p[1];
            }
        }
        let [a, b, c] = solve3(m).ok_or("no level plane")?;
        let n = [-a, 1.0, -b];
        normal = times(n, 1.0 / length(n));
        height = dot(normal, [around[0], c, around[2]]);
    }
    Ok(Table { normal, height, points: count })
}

fn solve3(mut m: [[f64; 4]; 3]) -> Option<V3> {
    for i in 0..3 {
        let pivot = (i..3).max_by(|&a, &b| m[a][i].abs().total_cmp(&m[b][i].abs()))?;
        m.swap(i, pivot);
        if m[i][i].abs() < 1e-12 {
            return None;
        }
        for row in 0..3 {
            if row != i {
                let f = m[row][i] / m[i][i];
                let source = m[i];
                for (value, s) in m[row].iter_mut().zip(source) {
                    *value -= f * s;
                }
            }
        }
    }
    Some([m[0][3] / m[0][0], m[1][3] / m[1][1], m[2][3] / m[2][2]])
}

/// p -> s R p + t, world to model.
#[derive(Clone, Copy)]
struct Placement {
    rotation: [V3; 3],
    translation: V3,
    scale: f64,
}

impl Placement {
    fn apply(&self, p: V3) -> V3 {
        let r = &self.rotation;
        add(times([dot(r[0], p), dot(r[1], p), dot(r[2], p)], self.scale), self.translation)
    }

    /// The small motion (rotation `w`, shift `shift`, growth `grow`) about `about`, after this.
    fn then(&self, w: V3, shift: V3, grow: f64, about: V3) -> Placement {
        let angle = length(w);
        let (s, c) = (angle.sin(), angle.cos());
        let k = if angle > 1e-12 { times(w, 1.0 / angle) } else { [1.0, 0.0, 0.0] };
        let turn = |v: V3| add(add(times(v, c), times(cross(k, v), s)), times(k, dot(k, v) * (1.0 - c)));
        let columns = [0, 1, 2].map(|j| turn([self.rotation[0][j], self.rotation[1][j], self.rotation[2][j]]));
        let rotation = [0, 1, 2].map(|i| [columns[0][i], columns[1][i], columns[2][i]]);
        let translation = add(add(about, times(turn(sub(self.translation, about)), 1.0 + grow)), shift);
        Placement { rotation, translation, scale: self.scale * (1.0 + grow) }
    }
}

/// Rows of a turn by `yaw` about the model's up.
fn yaw_rotation(yaw: f64) -> [V3; 3] {
    let (s, c) = yaw.sin_cos();
    [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]]
}

fn solve(a: &mut [Vec<f64>], b: &mut [f64]) -> Option<Vec<f64>> {
    let n = b.len();
    for i in 0..n {
        let pivot = (i..n).max_by(|&x, &y| a[x][i].abs().total_cmp(&a[y][i].abs()))?;
        a.swap(i, pivot);
        b.swap(i, pivot);
        if a[i][i].abs() < 1e-12 {
            return None;
        }
        for row in i + 1..n {
            let f = a[row][i] / a[i][i];
            let source = a[i].clone();
            for (value, s) in a[row].iter_mut().zip(&source) {
                *value -= f * s;
            }
            b[row] -= f * b[i];
        }
    }
    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        let tail: f64 = (i + 1..n).map(|j| a[i][j] * x[j]).sum();
        x[i] = (b[i] - tail) / a[i][i];
    }
    Some(x)
}

/// One point-to-plane step over the points within `trim` of the model; with `free_scale` also the
/// scale about their centre. None when too few points are near.
fn step(points: &[V3], mesh: &Mesh, placement: &Placement, trim: f64, free_scale: bool) -> Option<Placement> {
    let pairs: Vec<(V3, Hit)> = map_points(points, |p| { let q = placement.apply(p); mesh.closest(q, trim).map(|hit| (q, hit)) })
        .into_iter()
        .flatten()
        .collect();
    if pairs.len() < 50 {
        return None;
    }
    let about = times(pairs.iter().fold([0.0; 3], |sum, (q, _)| add(sum, *q)), 1.0 / pairs.len() as f64);
    let n = if free_scale { 7 } else { 6 };
    let (mut a, mut b) = (vec![vec![0.0; n]; n], vec![0.0; n]);
    for (q, hit) in &pairs {
        let arm = sub(*q, about);
        let (w, residual) = (cross(arm, hit.normal), dot(sub(*q, hit.point), hit.normal));
        let row = [w[0], w[1], w[2], hit.normal[0], hit.normal[1], hit.normal[2], dot(arm, hit.normal)];
        for i in 0..n {
            for j in 0..n {
                a[i][j] += row[i] * row[j];
            }
            b[i] -= row[i] * residual;
        }
    }
    for (i, row) in a.iter_mut().enumerate() {
        row[i] += 1e-9;
    }
    let x = solve(&mut a, &mut b)?;
    Some(placement.then([x[0], x[1], x[2]], [x[3], x[4], x[5]], if free_scale { x[6] } else { 0.0 }, about))
}

/// The mean distance to the model, each counted at most `cap`.
fn capped_mean(points: &[V3], mesh: &Mesh, placement: &Placement, cap: f64) -> f64 {
    let distances = map_points(points, |p| mesh.closest(placement.apply(p), cap).map_or(cap, |h| h.distance));
    distances.iter().sum::<f64>() / distances.len().max(1) as f64
}

/// The distances of the object's points to the model, millimetres.
#[derive(Clone, Copy, Debug, Default)]
pub struct Distances {
    pub p50_mm: f64,
    pub p95_mm: f64,
    pub mean_mm: f64,
    /// Outside the model positive: a reconstruction that grew or shrank.
    pub signed_mm: f64,
}

/// What counts as far, millimetres: a point beyond it is not the object's surface but counts there.
const FAR_MM: f64 = 50.0;

fn distances(points: &[V3], mesh: &Mesh, placement: &Placement) -> (Distances, Vec<f64>) {
    let far = FAR_MM / 1000.0;
    let hits = map_points(points, |p| {
        let q = placement.apply(p);
        mesh.closest(q, far).map_or((far, 0.0), |h| (h.distance, dot(sub(q, h.point), h.normal)))
    });
    let mut sorted: Vec<f64> = hits.iter().map(|h| h.0 * 1000.0).collect();
    let all = sorted.clone();
    sorted.sort_by(f64::total_cmp);
    let at = |q: f64| sorted.get((q * sorted.len().saturating_sub(1) as f64).round() as usize).copied().unwrap_or(0.0);
    let count = hits.len().max(1) as f64;
    let result = Distances {
        p50_mm: at(0.5),
        p95_mm: at(0.95),
        mean_mm: sorted.iter().sum::<f64>() / count,
        signed_mm: hits.iter().map(|h| h.1 * 1000.0).sum::<f64>() / count,
    };
    (result, all)
}

/// The fit's result. `model_points`: the object's points in the model's frame, what is kept of a scan.
pub struct ObjectFit {
    pub table: Table,
    pub object_points: usize,
    pub yaw_deg: f64,
    pub all: Distances,
    /// Of the points within 5 mm of the model, and their share of all.
    pub near: Distances,
    pub near_share: f64,
    /// The model's visible surface with a point within 1 mm.
    pub coverage_1mm: f64,
    /// The scale that fits best, less 1, in parts per million, and the distances at it.
    pub scale_ppm: f64,
    pub scaled: Distances,
    pub model_points: Vec<[f32; 3]>,
}

const YAW_STEPS: usize = 36;
const YAW_KEPT: usize = 4;

fn every_nth(points: &[V3], most: usize) -> Vec<V3> {
    points.iter().copied().step_by((points.len() / most.max(1)).max(1)).collect()
}

/// The object's points on `table`: above it, up to the model's height, around the middle they gather on.
fn cut_object(points: &[V3], table: &Table, around: V3, mesh: &Mesh) -> Vec<V3> {
    const CLEAR_OF_TABLE: f64 = 0.003;
    let extent = sub(mesh.high, mesh.low);
    let tall = extent[1] + 0.01;
    let reach = 0.65 * (extent[0] * extent[0] + extent[2] * extent[2]).sqrt();
    let above: Vec<V3> = points
        .iter()
        .copied()
        .filter(|&p| { let h = dot(table.normal, p) - table.height; h > CLEAR_OF_TABLE && h < tall })
        .collect();
    let mut middle = [around[0], around[2]];
    for radius in [3.0 * reach, 2.0 * reach, 1.5 * reach, reach, reach] {
        let mut xs: Vec<f64> = Vec::new();
        let mut zs: Vec<f64> = Vec::new();
        for p in &above {
            if (p[0] - middle[0]).hypot(p[2] - middle[1]) < radius {
                xs.push(p[0]);
                zs.push(p[2]);
            }
        }
        if xs.is_empty() {
            break;
        }
        xs.sort_by(f64::total_cmp);
        zs.sort_by(f64::total_cmp);
        middle = [xs[xs.len() / 2], zs[zs.len() / 2]];
    }
    above.into_iter().filter(|p| (p[0] - middle[0]).hypot(p[2] - middle[1]) < reach).collect()
}

/// What stands on the table where the wearer looked, measured with no model of it.
pub struct ObjectSize {
    pub table: Table,
    pub object_points: usize,
    /// The smallest box standing on the table around it, millimetres: its longer side, its shorter side, its height.
    pub size_mm: [f64; 3],
    /// The object's points in that box's frame, metres: x along the longer side, y up from the table, z along the
    /// shorter, the box's middle over the origin.
    pub points: Vec<[f32; 3]>,
}

/// The table's cells an object is told from its neighbours by, metres.
const OBJECT_CELL: f64 = 0.005;
/// What stands higher than this over the table is the room's, not the object's.
const OBJECT_TALLEST: f64 = 0.5;
/// The share of the points each end of a side may lose: a stray point does not set the size.
const SIZE_TRIM: f64 = 0.002;
/// How far round a point its neighbours say where the surface is, metres: the scan's noise scatters a wall's points
/// both ways across it, and the outermost of them, not the wall, would set the size.
const SURFACE_REACH: f64 = 0.003;

/// Each of `points` where the surface is at it: the mean of the points of `all` within `reach`.
fn on_surface(points: &[V3], all: &[V3], reach: f64) -> Vec<V3> {
    let cell = |p: V3| p.map(|v| (v / reach).floor() as i64);
    let mut cells: std::collections::HashMap<[i64; 3], Vec<V3>> = std::collections::HashMap::new();
    for &p in all {
        cells.entry(cell(p)).or_default().push(p);
    }
    map_points(points, |p| {
        let [x, y, z] = cell(p);
        let (mut sum, mut count) = ([0.0; 3], 0.0);
        for list in (x - 1..=x + 1).flat_map(|i| (y - 1..=y + 1).flat_map(move |j| (z - 1..=z + 1).map(move |k| [i, j, k]))).filter_map(|c| cells.get(&c)) {
            for &q in list.iter().filter(|&&q| length(sub(q, p)) <= reach) {
                sum = add(sum, q);
                count += 1.0;
            }
        }
        if count > 0.0 { times(sum, 1.0 / count) } else { p }
    })
}

/// How far in from a side's outermost points its face is looked for, metres: a face across the side holds its points
/// there, spread by the noise the surface's mean left (the outermost of them lie out in that spread); a curved side
/// reads at most half of this in.
const FACE_DEPTH: f64 = 0.001;

/// Where `values` end, both ways: the middle of the points within FACE_DEPTH of each trimmed extreme.
fn ends(mut values: Vec<f64>) -> (f64, f64) {
    values.sort_by(f64::total_cmp);
    let last = values.len().saturating_sub(1) as f64;
    let (low, high) = (values[(SIZE_TRIM * last).round() as usize], values[((1.0 - SIZE_TRIM) * last).round() as usize]);
    let low_face = &values[..values.partition_point(|&v| v <= low + FACE_DEPTH)];
    let high_face = &values[values.partition_point(|&v| v < high - FACE_DEPTH)..];
    (low_face[low_face.len() / 2], high_face[high_face.len() / 2])
}

/// The span of `values` between their ends, and its middle.
fn span(values: Vec<f64>) -> (f64, f64) {
    let (low, high) = ends(values);
    (high - low, 0.5 * (low + high))
}

/// The object on the table nearest `around`: the points over the table that touch each other from there on, and
/// the smallest box on the table that holds them.
pub fn measure_object(points: &[V3], around: V3) -> Result<ObjectSize, String> {
    const CLEAR_OF_TABLE: f64 = 0.003;
    let table = find_table(points, around)?;
    let n = table.normal;
    // the table's own axes: u nearest the world's x, v across
    let u = { let u = sub([1.0, 0.0, 0.0], times(n, n[0])); times(u, 1.0 / length(u)) };
    let v = cross(u, n);
    let on_table = |p: V3| [dot(u, p), dot(n, p) - table.height, dot(v, p)];
    let above: Vec<V3> = points.iter().map(|&p| on_table(p)).filter(|q| q[1] > CLEAR_OF_TABLE && q[1] < OBJECT_TALLEST).collect();
    let cell = |q: &V3| ((q[0] / OBJECT_CELL).floor() as i64, (q[2] / OBJECT_CELL).floor() as i64);
    let mut filled: std::collections::HashMap<(i64, i64), (usize, bool)> = std::collections::HashMap::new();
    for q in &above {
        filled.entry(cell(q)).or_insert((0, false)).0 += 1;
    }
    // one point does not fill a cell: a stray one bridges nothing
    filled.retain(|_, c| c.0 >= 2);
    let middle = on_table(around);
    let distance = |c: &(i64, i64)| ((c.0 as f64 + 0.5) * OBJECT_CELL - middle[0]).hypot((c.1 as f64 + 0.5) * OBJECT_CELL - middle[2]);
    let start = *filled.keys().min_by(|a, b| distance(a).total_cmp(&distance(b)).then(a.cmp(b))).ok_or("nothing stands on the table")?;
    let mut open = vec![start];
    filled.get_mut(&start).expect("the start is filled").1 = true;
    while let Some((x, z)) = open.pop() {
        for next in (-1..=1).flat_map(|dx| (-1..=1).map(move |dz| (x + dx, z + dz))) {
            if let Some(c) = filled.get_mut(&next) {
                if !c.1 {
                    c.1 = true;
                    open.push(next);
                }
            }
        }
    }
    let object: Vec<V3> = above.into_iter().filter(|q| filled.get(&cell(q)).is_some_and(|c| c.1)).collect();
    if object.len() < 500 {
        return Err(format!("{} points on the object", object.len()));
    }
    let few = on_surface(&every_nth(&object, 20_000), &object, SURFACE_REACH);
    let sides = |yaw: f64| {
        let (s, c) = yaw.sin_cos();
        (span(few.iter().map(|q| c * q[0] + s * q[2]).collect()), span(few.iter().map(|q| c * q[2] - s * q[0]).collect()))
    };
    // a box's sides repeat every quarter turn
    let yaw = (0..180)
        .map(|i| f64::from(i).to_radians() * 0.5)
        .min_by(|&a, &b| { let (x, y) = (sides(a), sides(b)); (x.0 .0 * x.1 .0).total_cmp(&(y.0 .0 * y.1 .0)) })
        .expect("a yaw");
    let ((along, along_middle), (across, across_middle)) = sides(yaw);
    let (_, height) = ends(few.iter().map(|q| q[1]).collect());
    let (s, c) = yaw.sin_cos();
    let turned = |q: &V3| [c * q[0] + s * q[2] - along_middle, q[1], c * q[2] - s * q[0] - across_middle];
    let longer_first = along >= across;
    let points = object
        .iter()
        .map(|q| { let t = turned(q); if longer_first { [t[0] as f32, t[1] as f32, t[2] as f32] } else { [t[2] as f32, t[1] as f32, -t[0] as f32] } })
        .collect();
    Ok(ObjectSize { table, object_points: object.len(), size_mm: [1000.0 * along.max(across), 1000.0 * along.min(across), 1000.0 * height], points })
}

/// Fits the model standing on the table near `around`, turned about its up, to the points.
pub fn fit_object(points: &[V3], around: V3, mesh: &Mesh) -> Result<ObjectFit, String> {
    let table = find_table(points, around)?;
    let object = cut_object(points, &table, around, mesh);
    if object.len() < 500 {
        return Err(format!("{} points on the object", object.len()));
    }
    let middle = times(object.iter().fold([0.0; 3], |sum, p| add(sum, *p)), 1.0 / object.len() as f64);
    let foot = [middle[0], (table.height - table.normal[0] * middle[0] - table.normal[2] * middle[2]) / table.normal[1], middle[2]];
    let model_foot = [0.5 * (mesh.low[0] + mesh.high[0]), mesh.low[1], 0.5 * (mesh.low[2] + mesh.high[2])];
    let place = |yaw: f64| {
        let rotation = yaw_rotation(yaw);
        let turned = [dot(rotation[0], foot), dot(rotation[1], foot), dot(rotation[2], foot)];
        Placement { rotation, translation: sub(model_foot, turned), scale: 1.0 }
    };
    let (few, many) = (every_nth(&object, 1500), every_nth(&object, 40_000));
    let mut yaws: Vec<(f64, f64)> = (0..YAW_STEPS)
        .map(|i| {
            let yaw = i as f64 / YAW_STEPS as f64 * std::f64::consts::TAU;
            (capped_mean(&few, mesh, &place(yaw), 0.01), yaw)
        })
        .collect();
    yaws.sort_by(|a, b| a.0.total_cmp(&b.0));
    let refine = |start: Placement, trims: &[f64], free_scale: bool| {
        let mut current = start;
        for &trim in trims {
            for _ in 0..4 {
                if let Some(next) = step(&many, mesh, &current, trim, free_scale) {
                    current = next;
                }
            }
        }
        current
    };
    let mut best: Option<(f64, f64, Placement)> = None;
    for &(_, yaw) in yaws.iter().take(YAW_KEPT) {
        let placement = refine(place(yaw), &[0.02, 0.01, 0.006, 0.004], false);
        let score = capped_mean(&many, mesh, &placement, 0.004);
        if best.as_ref().is_none_or(|b| score < b.0) {
            best = Some((score, yaw, placement));
        }
    }
    let (_, yaw, placement) = best.ok_or("no placement")?;
    let placement = refine(placement, &[0.003], false);
    let (all, each) = distances(&object, mesh, &placement);
    let near_points: Vec<V3> = object.iter().zip(&each).filter(|(_, &d)| d <= 5.0).map(|(p, _)| *p).collect();
    let near_share = near_points.len() as f64 / object.len() as f64;
    let (near, _) = distances(&near_points, mesh, &placement);
    let coverage_1mm = coverage(&near_points, mesh, &placement);
    let scaled_placement = refine(placement, &[0.003, 0.003], true);
    let (scaled, _) = distances(&object, mesh, &scaled_placement);
    let model_points = object
        .iter()
        .map(|&p| { let q = placement.apply(p); [q[0] as f32, q[1] as f32, q[2] as f32] })
        .collect();
    Ok(ObjectFit {
        table,
        object_points: object.len(),
        yaw_deg: yaw.to_degrees(),
        all,
        near,
        near_share,
        coverage_1mm,
        scale_ppm: (scaled_placement.scale - 1.0) * 1e6,
        scaled,
        model_points,
    })
}

/// The share of the model's visible surface samples with a point within 1 mm.
fn coverage(points: &[V3], mesh: &Mesh, placement: &Placement) -> f64 {
    const REACH: f64 = 0.001;
    const SAMPLES: usize = 20_000;
    let cell_of = |p: V3| p.map(|v| (v / REACH).floor() as i32);
    let mut cells: std::collections::HashMap<[i32; 3], Vec<V3>> = std::collections::HashMap::new();
    for &p in points {
        let q = placement.apply(p);
        cells.entry(cell_of(q)).or_default().push(q);
    }
    let samples = mesh.surface_samples(SAMPLES);
    let covered = map_points(&samples, |s| {
        let [x, y, z] = cell_of(s);
        (x - 1..=x + 1).any(|i| (y - 1..=y + 1).any(|j| (z - 1..=z + 1).any(|k| {
            cells.get(&[i, j, k]).is_some_and(|list| list.iter().any(|&q| length(sub(q, s)) <= REACH))
        })))
    });
    covered.iter().filter(|&&c| c).count() as f64 / samples.len().max(1) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A box standing on y = 0, sized in metres, as twelve triangles facing out.
    fn box_triangles(size: V3) -> Vec<[V3; 3]> {
        let [x, y, z] = size;
        let v = |i: u8, j: u8, k: u8| [f64::from(i) * x - x / 2.0, f64::from(j) * y, f64::from(k) * z - z / 2.0];
        let quad = |a: V3, b: V3, c: V3, d: V3| [[a, b, c], [a, c, d]];
        [
            quad(v(0, 0, 1), v(1, 0, 1), v(1, 1, 1), v(0, 1, 1)),
            quad(v(1, 0, 0), v(0, 0, 0), v(0, 1, 0), v(1, 1, 0)),
            quad(v(1, 0, 1), v(1, 0, 0), v(1, 1, 0), v(1, 1, 1)),
            quad(v(0, 0, 0), v(0, 0, 1), v(0, 1, 1), v(0, 1, 0)),
            quad(v(0, 1, 1), v(1, 1, 1), v(1, 1, 0), v(0, 1, 0)),
            quad(v(0, 0, 0), v(1, 0, 0), v(1, 0, 1), v(0, 0, 1)),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// The box's sides and top sampled, moved by a yaw and a shift, with a table around it.
    fn scan_of(mesh: &Mesh, yaw: f64, shift: V3, scale: f64) -> Vec<V3> {
        let r = yaw_rotation(yaw);
        // model to world: the rows' transpose
        let world = |q: V3| add(times([r[0][0] * q[0] + r[1][0] * q[1] + r[2][0] * q[2], r[0][1] * q[0] + r[1][1] * q[1] + r[2][1] * q[2], r[0][2] * q[0] + r[1][2] * q[1] + r[2][2] * q[2]], scale), shift);
        let mut points: Vec<V3> = mesh.surface_samples(30_000).into_iter().map(world).collect();
        let mut random = Random(3);
        for _ in 0..60_000 {
            let (x, z) = (shift[0] + 0.7 * (random.unit() - 0.5), shift[2] + 0.7 * (random.unit() - 0.5));
            points.push([x, shift[1], z]);
        }
        points
    }

    #[test]
    fn the_nearest_point_is_found_on_any_face_or_not_at_all() {
        let mesh = Mesh::new(box_triangles([0.1, 0.08, 0.06])).unwrap();
        let hit = mesh.closest([0.0, 0.04, 0.05], 0.1).unwrap();
        assert!((hit.distance - 0.02).abs() < 1e-9 && (hit.normal[2] - 1.0).abs() < 1e-9, "{}", hit.distance);
        let inside = mesh.closest([0.045, 0.04, 0.0], 0.1).unwrap();
        assert!((inside.distance - 0.005).abs() < 1e-9 && (inside.normal[0] - 1.0).abs() < 1e-9);
        let corner = mesh.closest([0.06, 0.09, 0.04], 0.1).unwrap();
        assert!((corner.distance - (3.0_f64 * 1e-4).sqrt()).abs() < 1e-9, "{}", corner.distance);
        assert!(mesh.closest([0.5, 0.0, 0.0], 0.1).is_none());
    }

    #[test]
    fn a_binary_stl_stands_up_in_metres() {
        let mut bytes = vec![0u8; 80];
        bytes.extend(1u32.to_le_bytes());
        bytes.extend([0u8; 12]);
        for corner in [[0.0f32, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 20.0, 30.0]] {
            for v in corner {
                bytes.extend(v.to_le_bytes());
            }
        }
        bytes.extend([0u8; 2]);
        let mesh = Mesh::from_stl(&bytes).unwrap();
        assert_eq!(mesh.high, [0.01, 0.03, 0.0]);
        assert_eq!(mesh.low, [0.0, 0.0, -0.02]);
        bytes.pop();
        assert!(Mesh::from_stl(&bytes).is_err());
    }

    #[test]
    fn a_scan_is_found_on_its_table_and_fits_its_model() {
        let mesh = Mesh::new(box_triangles([0.1, 0.08, 0.06])).unwrap();
        let shift = [0.4, 0.75, -0.3];
        let points = scan_of(&mesh, 0.7, shift, 1.0);
        let looked = [shift[0] + 0.03, shift[1] + 0.03, shift[2] - 0.02];
        let fit = fit_object(&points, looked, &mesh).unwrap();
        assert!((fit.table.height - shift[1]).abs() < 1e-3 && fit.table.normal[1] > 0.999, "{}", fit.table.height);
        assert!(fit.table.points > 40_000, "{}", fit.table.points);
        assert!(fit.all.p95_mm < 0.05 && fit.near_share > 0.999, "{:?}", fit.all);
        assert!(fit.scale_ppm.abs() < 1000.0, "{}", fit.scale_ppm);
        assert!(fit.coverage_1mm > 0.8, "{}", fit.coverage_1mm);
        // the points the table hid are gone, and only the object's are kept
        assert!(fit.model_points.len() < 30_000 && fit.object_points == fit.model_points.len());
    }

    #[test]
    fn the_fit_reports_the_scale_it_was_given() {
        let mesh = Mesh::new(box_triangles([0.1, 0.08, 0.06])).unwrap();
        let shift = [-0.2, 0.7, 0.5];
        let points = scan_of(&mesh, 2.1, shift, 1.01);
        let fit = fit_object(&points, shift, &mesh).unwrap();
        assert!((fit.scale_ppm + 9901.0).abs() < 1000.0, "{}", fit.scale_ppm);
        assert!(fit.scaled.p95_mm < fit.all.p95_mm, "{:?} {:?}", fit.scaled, fit.all);
    }

    #[test]
    fn an_object_is_measured_with_no_model_and_its_neighbour_left_out() {
        let mesh = Mesh::new(box_triangles([0.1, 0.08, 0.06])).unwrap();
        let shift = [0.4, 0.75, -0.3];
        let mut points = scan_of(&mesh, 0.7, shift, 1.0);
        let only = points.len();
        // a second thing on the table, 25 cm on
        points.extend(scan_of(&Mesh::new(box_triangles([0.05, 0.2, 0.05])).unwrap(), 0.0, add(shift, [0.25, 0.0, 0.0]), 1.0).into_iter().take(30_000));
        let size = measure_object(&points, [shift[0] + 0.02, shift[1] + 0.03, shift[2] - 0.01]).unwrap();
        let [long, short, tall] = size.size_mm;
        assert!((long - 100.0).abs() < 1.0 && (short - 60.0).abs() < 1.0, "the footprint: {long} x {short}");
        assert!((tall - 80.0).abs() < 1.0, "the height: {tall}");
        assert!(size.object_points < only - 60_000, "the neighbour or the table was counted: {}", size.object_points);
        let widest = size.points.iter().map(|p| p[0].abs()).fold(0.0, f32::max);
        assert!((widest - 0.05).abs() < 1e-3, "the kept points lie along the longer side about its middle: {widest}");
        // the scan's noise, about 0.5 mm each way, does not make it bigger
        let mut random = Random(7);
        let noisy: Vec<V3> = points.iter().map(|p| p.map(|v| v + 0.001 * (random.unit() + random.unit() + random.unit() - 1.5))).collect();
        let [long, short, tall] = measure_object(&noisy, [shift[0] + 0.02, shift[1] + 0.03, shift[2] - 0.01]).unwrap().size_mm;
        assert!((long - 100.0).abs() < 0.5 && (short - 60.0).abs() < 0.5 && (tall - 80.0).abs() < 0.5, "with noise: {long} x {short} x {tall}");
    }
}
