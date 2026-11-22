//! What the Force render mode reads and draws. The person looks at the real part: its shade reads
//! a solve's fields at the point it hit ([`Values`], a regular lattice, so a finer solve sharpens
//! the colour and never the shape) and the loads' regions the same way ([`Marks`]). Only the
//! exaggerated deformation needs a mesh: the analysis surface with each vertex's fields and
//! displacement ([`Display`]), and where a ray meets it as drawn.

use crate::element::V3;
use crate::grid::Grid;
use crate::surface::{ray_triangle, Surface};
use crate::Outcome;

/// The fields each vertex carries, in [`Display::values`]' order.
pub const VON_MISES: usize = 0;
pub const PRINCIPAL: usize = 1;
/// How much of its strength the material uses: 1 / the safety factor (0 unstressed).
pub const UTILISATION: usize = 2;
pub const DISPLACEMENT: usize = 3;

/// How many nodes out from the analysed cells a solve's fields reach: the real surface runs up
/// to a cell off the cells the study counts as filled. Past that a point has no value: a piece
/// nothing holds, or a detail the cells did not see, is not analysed.
const VALUES_REACH: usize = 2;

/// A solve's fields at its grid's nodes, read trilinear at any point of the part.
#[derive(Clone, Debug, Default)]
pub struct Values {
    /// The corner of cell (0, 0, 0), the cells' side and how many per axis.
    pub origin: V3,
    pub h: f64,
    pub cells: [usize; 3],
    /// Per node, x fastest: the fields in [`Display::values`]' order. Infinite: no value here.
    pub nodes: Vec<[f32; 4]>,
}

impl Values {
    pub fn of(o: &Outcome) -> Values {
        let (g, r) = (&o.grid, &o.results);
        let von_mises = crate::results::nodal_by(g, r, |c| c[0]);
        let principal = crate::results::nodal_by(g, r, |c| c[1]);
        let utilisation = crate::results::nodal_by(g, r, |c| if c[2].is_finite() && c[2] > 0.0 { 1.0 / c[2] } else { 0.0 });
        let moved = |n: usize| {
            let u = o.solution.u[n];
            (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt() as f32
        };
        let mut nodes: Vec<[f32; 4]> = (0..g.nodes())
            .map(|n| match von_mises[n].is_finite() && principal[n].is_finite() && utilisation[n].is_finite() {
                true => [von_mises[n], principal[n], utilisation[n], moved(n)],
                false => [f32::INFINITY; 4],
            })
            .collect();
        spread(&mut nodes, g.node_dims());
        Values { origin: g.origin, h: g.h[0], cells: g.n, nodes }
    }

    /// The fields at a point, over the nodes round it that have them.
    pub fn at(&self, p: V3) -> Option<[f32; 4]> {
        let (mut g, mut t) = ([0usize; 3], [0.0f64; 3]);
        for a in 0..3 {
            let f = (p[a] - self.origin[a]) / self.h;
            g[a] = (f.floor().max(0.0) as usize).min(self.cells[a] - 1);
            t[a] = (f - g[a] as f64).clamp(0.0, 1.0);
        }
        let (dx, dy) = (self.cells[0] + 1, self.cells[1] + 1);
        let (mut sum, mut weight) = ([0.0f64; 4], 0.0f64);
        for c in 0..8usize {
            let (i, j, k) = (g[0] + (c & 1), g[1] + ((c >> 1) & 1), g[2] + (c >> 2));
            let v = self.nodes[i + dx * (j + dy * k)];
            let w = (if c & 1 == 1 { t[0] } else { 1.0 - t[0] }) * (if c & 2 == 2 { t[1] } else { 1.0 - t[1] }) * (if c & 4 == 4 { t[2] } else { 1.0 - t[2] });
            if v[0].is_finite() {
                weight += w;
                (0..4).for_each(|f| sum[f] += w * v[f] as f64);
            }
        }
        (weight > 0.0).then(|| sum.map(|s| (s / weight) as f32))
    }

    /// One field as a colour ramp reads it, per node: its share of `top`, or with `signed` -`top`
    /// to `top` over 0..1. Negative: no value.
    pub fn over_ramp(&self, field: usize, top: f32, signed: bool) -> Vec<f32> {
        let top = top.max(1e-12);
        self.nodes
            .iter()
            .map(|v| match (v[0].is_finite(), signed) {
                (false, _) => -1.0,
                (true, true) => (0.5 + 0.5 * v[field] / top).max(0.0),
                (true, false) => (v[field] / top).max(0.0),
            })
            .collect()
    }
}

/// Fills the nodes with no value up to [`VALUES_REACH`] nodes out from those that have one, each
/// the mean of its neighbours along the axes.
fn spread(nodes: &mut [[f32; 4]], dim: [usize; 3]) {
    let (dx, dy) = (dim[0], dim[0] * dim[1]);
    for _ in 0..VALUES_REACH {
        let mut filled = Vec::new();
        for k in 0..dim[2] {
            for j in 0..dim[1] {
                for i in 0..dim[0] {
                    let n = i + dx * j + dy * k;
                    if nodes[n][0].is_finite() {
                        continue;
                    }
                    let (mut sum, mut count) = ([0.0f32; 4], 0.0f32);
                    let mut add = |near: usize| {
                        if nodes[near][0].is_finite() {
                            (0..4).for_each(|f| sum[f] += nodes[near][f]);
                            count += 1.0;
                        }
                    };
                    if i > 0 { add(n - 1); }
                    if i + 1 < dim[0] { add(n + 1); }
                    if j > 0 { add(n - dx); }
                    if j + 1 < dim[1] { add(n + dx); }
                    if k > 0 { add(n - dy); }
                    if k + 1 < dim[2] { add(n + dy); }
                    if count > 0.0 {
                        filled.push((n, sum.map(|s| s / count)));
                    }
                }
            }
        }
        filled.into_iter().for_each(|(n, v)| nodes[n] = v);
    }
}

/// How far a surface's normal turns from a marked one and still lies along it: past this it is
/// another face through the same cell. The shader's test (`overlay_mark`) has the same number.
const MARK_COS: f64 = 0.6;

/// Where the loads' regions lie on a study's cells, read at a point of the real part the way
/// [`Values`] is: a cell holds a kind and the normal of the surface marked in it, so a mark stops
/// at a crease of the real part and not at a cell's side.
#[derive(Clone, Debug, Default)]
pub struct Marks {
    pub origin: V3,
    pub h: f64,
    pub cells: [usize; 3],
    /// Per cell, x fastest: 0, or a kind + 16 (x + 128 y), the normal's x and y with z up at
    /// 0..=127 over -1..1 (`fab_paint_field::bake::Overlay`).
    pub marks: Vec<f32>,
    /// Anything is marked.
    pub any: bool,
}

impl Marks {
    pub fn new(grid: &Grid) -> Marks {
        Marks { origin: grid.origin, h: grid.h[0], cells: grid.n, marks: vec![0.0; grid.cells()], any: false }
    }

    fn cell_of(&self, p: V3) -> [usize; 3] {
        std::array::from_fn(|a| (((p[a] - self.origin[a]) / self.h).floor().max(0.0) as usize).min(self.cells[a] - 1))
    }

    /// Marks `faces` of `surface` as `kind` (1..=8), over what is marked: the cells they lie in and
    /// the ones round them not marked yet (the real surface runs up to a cell off the study's).
    pub fn mark(&mut self, surface: &Surface, faces: &[u32], kind: u16) {
        let [nx, ny, nz] = self.cells;
        let mut around = Vec::with_capacity(faces.len());
        for &q in faces {
            let c = surface.quad_center[q as usize].map(|v| v as f64);
            let [i, j, k] = self.cell_of(c);
            let value = encode(kind, surface.quad_normal[q as usize]);
            self.marks[i + nx * (j + ny * k)] = value;
            around.push(([i, j, k], value));
        }
        for ([i, j, k], value) in around {
            for z in k.saturating_sub(1)..(k + 2).min(nz) {
                for y in j.saturating_sub(1)..(j + 2).min(ny) {
                    for x in i.saturating_sub(1)..(i + 2).min(nx) {
                        let m = &mut self.marks[x + nx * (y + ny * z)];
                        if *m == 0.0 {
                            *m = value;
                        }
                    }
                }
            }
        }
        self.any |= !faces.is_empty();
    }

    /// The kind marked at a point of a surface with normal `n` (0: none).
    pub fn at(&self, p: V3, n: V3) -> u16 {
        let [i, j, k] = self.cell_of(p);
        let m = self.marks[i + self.cells[0] * (j + self.cells[1] * k)];
        if m == 0.0 {
            return 0;
        }
        let m = m as u32;
        let (x, y) = ((m >> 4) & 127, m >> 11);
        let (x, y) = (x as f64 / 127.0 * 2.0 - 1.0, y as f64 / 127.0 * 2.0 - 1.0);
        let z = (1.0 - x * x - y * y).max(0.0).sqrt();
        let along = (x * n[0] + y * n[1] + z * n[2]).abs() / (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-12);
        if along < MARK_COS { 0 } else { (m & 15) as u16 }
    }
}

/// A mark's value: its kind and its surface's normal with z up.
fn encode(kind: u16, n: [f32; 3]) -> f32 {
    let s = if n[2] < 0.0 { -1.0 } else { 1.0 };
    let bits = |v: f32| ((s * v * 0.5 + 0.5).clamp(0.0, 1.0) * 127.0).round() as u32;
    (kind as u32 + 16 * (bits(n[0]) + 128 * bits(n[1]))) as f32
}

#[derive(Clone, Debug, Default)]
pub struct Display {
    pub pos: Vec<[f32; 3]>,
    pub normal: Vec<[f32; 3]>,
    /// Each vertex's displacement (mm).
    pub disp: Vec<[f32; 3]>,
    /// Von Mises (MPa), largest principal stress (MPa), utilisation, displacement (mm).
    pub values: Vec<[f32; 4]>,
    pub quads: Vec<[u32; 4]>,
    /// Each field's colour scale top: its 99.5th percentile over the vertices, so one
    /// concentrated corner does not wash the rest out (the readouts and [`Display::peak`] give
    /// the true extremes).
    pub scale: [f32; 4],
    /// Each field's largest over the whole part, as the panel's Largest rows read it; the
    /// utilisation's is the one at the lowest safety factor.
    pub peak: [f32; 4],
    /// The largest displacement (mm).
    pub max_u: f32,
}

impl Display {
    /// The surface with an analysis' fields.
    pub fn new(surface: &Surface, o: &Outcome, values: &Values) -> Display {
        let n = surface.pos.len();
        let mut d = Display {
            pos: surface.pos.clone(),
            normal: surface.normal.clone(),
            disp: vec![[0.0; 3]; n],
            values: vec![[0.0; 4]; n],
            quads: surface.quads.clone(),
            scale: [1.0; 4],
            peak: [o.results.max_vm.0 as f32, o.results.max_p1.0 as f32, (1.0 / o.results.min_sf.0) as f32, o.results.max_u.0 as f32],
            max_u: 0.0,
        };
        for (i, p) in surface.pos.iter().enumerate() {
            let q = p.map(|v| v as f64);
            let u = crate::results::displacement(&o.grid, &o.solution.u, q);
            let uf = u.map(|v| v as f32);
            let at = values.at(q).unwrap_or([0.0; 4]);
            let lu = (uf[0] * uf[0] + uf[1] * uf[1] + uf[2] * uf[2]).sqrt();
            d.disp[i] = uf;
            d.values[i] = [at[0], at[1], at[2], lu];
            d.max_u = d.max_u.max(lu);
        }
        for k in 0..4 {
            let mut v: Vec<f32> = d.values.iter().map(|x| if k == PRINCIPAL { x[k].abs() } else { x[k] }).collect();
            if v.is_empty() {
                continue;
            }
            let at = ((v.len() - 1) as f64 * 0.995) as usize;
            let (_, top, _) = v.select_nth_unstable_by(at, |a, b| a.total_cmp(b));
            d.scale[k] = if *top > 0.0 { *top } else { 1.0 };
        }
        d
    }

    /// The vertices where they show, `magnify` times their displacement off where they are.
    pub fn shown(&self, magnify: f32) -> Vec<[f32; 3]> {
        self.pos.iter().zip(&self.disp).map(|(p, u)| std::array::from_fn(|a| p[a] + magnify * u[a])).collect()
    }

    /// The fields at a point of quad `q` as drawn at `shown` (inverse-distance over its four
    /// vertices), and the point's place on the undeformed surface.
    pub fn at(&self, shown: &[[f32; 3]], q: u32, point: V3) -> ([f32; 4], V3) {
        let quad = self.quads[q as usize];
        let mut w = [0.0f64; 4];
        for (k, &v) in quad.iter().enumerate() {
            let p = shown[v as usize];
            let d = (0..3).map(|a| (p[a] as f64 - point[a]).powi(2)).sum::<f64>().sqrt();
            w[k] = 1.0 / (d + 1e-6);
        }
        let total: f64 = w.iter().sum();
        let mut values = [0.0f32; 4];
        let mut rest = [0.0f64; 3];
        for (k, &v) in quad.iter().enumerate() {
            let s = (w[k] / total) as f32;
            for f in 0..4 {
                values[f] += s * self.values[v as usize][f];
            }
            // the same blend of the vertices' own (undeformed) places
            for a in 0..3 {
                rest[a] += w[k] / total * (self.pos[v as usize][a] as f64 + (point[a] - shown[v as usize][a] as f64));
            }
        }
        (values, rest)
    }
}

/// A uniform grid of buckets over quads, so a ray meets only the quads near its path
/// (Amanatides and Woo, "A fast voxel traversal algorithm for ray tracing", 1987).
#[derive(Clone, Debug, Default)]
pub struct Bins {
    lo: V3,
    cell: f64,
    n: [usize; 3],
    /// Bucket b holds `items[start[b]..start[b + 1]]`.
    start: Vec<u32>,
    items: Vec<u32>,
}

impl Bins {
    /// Buckets over `quads` of vertices at `pos`, a few quads across each.
    pub fn new(pos: &[[f32; 3]], quads: &[[u32; 4]]) -> Bins {
        if quads.is_empty() {
            return Bins::default();
        }
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        let mut edge = 0.0;
        for q in quads {
            for &v in q {
                for a in 0..3 {
                    lo[a] = lo[a].min(pos[v as usize][a] as f64);
                    hi[a] = hi[a].max(pos[v as usize][a] as f64);
                }
            }
            let (p0, p2) = (pos[q[0] as usize], pos[q[2] as usize]);
            edge += (0..3).map(|a| (p0[a] - p2[a]) as f64 * (p0[a] - p2[a]) as f64).sum::<f64>().sqrt();
        }
        // a few quads across a bucket, and at most 256 buckets along an axis
        let widest = (0..3).map(|a| hi[a] - lo[a]).fold(0.0, f64::max);
        let cell = (4.0 * edge / quads.len() as f64).max(widest / 255.0).max(1e-3);
        let n: [usize; 3] = std::array::from_fn(|a| ((hi[a] - lo[a]) / cell).floor() as usize + 1);
        let bucket = |p: [f32; 3], a: usize| (((p[a] as f64 - lo[a]) / cell) as usize).min(n[a] - 1);
        let span = |q: &[u32; 4]| -> [(usize, usize); 3] {
            std::array::from_fn(|a| {
                let b = q.map(|v| bucket(pos[v as usize], a));
                (*b.iter().min().unwrap(), *b.iter().max().unwrap())
            })
        };
        let index = |i: usize, j: usize, k: usize| i + n[0] * (j + n[1] * k);
        let mut count = vec![0u32; n[0] * n[1] * n[2] + 1];
        for q in quads {
            let s = span(q);
            for k in s[2].0..=s[2].1 {
                for j in s[1].0..=s[1].1 {
                    for i in s[0].0..=s[0].1 {
                        count[index(i, j, k) + 1] += 1;
                    }
                }
            }
        }
        for b in 1..count.len() {
            count[b] += count[b - 1];
        }
        let mut fill = count.clone();
        let mut items = vec![0u32; *count.last().unwrap() as usize];
        for (qi, q) in quads.iter().enumerate() {
            let s = span(q);
            for k in s[2].0..=s[2].1 {
                for j in s[1].0..=s[1].1 {
                    for i in s[0].0..=s[0].1 {
                        let b = index(i, j, k);
                        items[fill[b] as usize] = qi as u32;
                        fill[b] += 1;
                    }
                }
            }
        }
        Bins { lo, cell, n, start: count, items }
    }

    /// The nearest quad the ray `origin + t dir` meets (vertices at `pos`) and where.
    pub fn hit(&self, pos: &[[f32; 3]], quads: &[[u32; 4]], origin: V3, dir: V3) -> Option<(u32, V3)> {
        if self.items.is_empty() {
            return None;
        }
        let hi: V3 = std::array::from_fn(|a| self.lo[a] + self.n[a] as f64 * self.cell);
        // where the ray is inside the buckets' box
        let (mut t0, mut t1) = (0.0f64, f64::MAX);
        for a in 0..3 {
            if dir[a].abs() < 1e-12 {
                if origin[a] < self.lo[a] || origin[a] > hi[a] {
                    return None;
                }
                continue;
            }
            let (ta, tb) = ((self.lo[a] - origin[a]) / dir[a], (hi[a] - origin[a]) / dir[a]);
            t0 = t0.max(ta.min(tb));
            t1 = t1.min(ta.max(tb));
        }
        if t0 > t1 {
            return None;
        }
        let start: V3 = std::array::from_fn(|a| origin[a] + t0 * dir[a]);
        let mut ijk: [isize; 3] = std::array::from_fn(|a| (((start[a] - self.lo[a]) / self.cell).floor() as isize).clamp(0, self.n[a] as isize - 1));
        let step: [isize; 3] = std::array::from_fn(|a| if dir[a] > 0.0 { 1 } else { -1 });
        let mut next: V3 = std::array::from_fn(|a| {
            if dir[a].abs() < 1e-12 {
                return f64::MAX;
            }
            let edge = self.lo[a] + (ijk[a] + if dir[a] > 0.0 { 1 } else { 0 }) as f64 * self.cell;
            (edge - origin[a]) / dir[a]
        });
        let delta: V3 = std::array::from_fn(|a| if dir[a].abs() < 1e-12 { f64::MAX } else { self.cell / dir[a].abs() });
        let mut best: Option<(f64, u32)> = None;
        loop {
            let b = ijk[0] as usize + self.n[0] * (ijk[1] as usize + self.n[1] * ijk[2] as usize);
            for &qi in &self.items[self.start[b] as usize..self.start[b + 1] as usize] {
                let p = quads[qi as usize].map(|v| pos[v as usize].map(|x| x as f64));
                for tri in [[p[0], p[1], p[2]], [p[0], p[2], p[3]]] {
                    if let Some(t) = ray_triangle(origin, dir, tri) {
                        if best.is_none_or(|(bt, _)| t < bt) {
                            best = Some((t, qi));
                        }
                    }
                }
            }
            let a = (0..3).min_by(|&x, &y| next[x].total_cmp(&next[y])).unwrap();
            // a hit nearer than the next bucket's entry is the nearest of all
            if best.is_some_and(|(t, _)| t <= next[a]) || next[a] > t1 {
                break;
            }
            ijk[a] += step[a];
            if ijk[a] < 0 || ijk[a] >= self.n[a] as isize {
                break;
            }
            next[a] += delta[a];
        }
        best.map(|(t, q)| (q, std::array::from_fn(|a| origin[a] + t * dir[a])))
    }
}
