//! Weak connections measured from the solid layer stack. Call on a worker.

use makepad_fab_slicer::clip::{intersection_ex, union_ex};
use makepad_fab_slicer::geom::{dist_point_segment, total_area_mm2, ExPolygon, Pt, SCALE};
use makepad_fab_toolpath::par;
use std::collections::{BTreeSet, BinaryHeap};
use std::cmp::Reverse;

#[derive(Clone, Debug, PartialEq)]
pub struct WeakSpot {
    /// The thinnest place (plate mm).
    pub at: [f64; 3],
    /// Well inside the run: what a limb's end has at `at`, its median section has here.
    pub middle: [f64; 3],
    /// The bottleneck's width (mm), under the requested thickness.
    pub thickness: f64,
    pub kind: SpotKind,
    /// The thin run's box (plate mm).
    pub bounds: ([f64; 3], [f64; 3]),
    /// Its principal axis, signed from the heavier side to the lighter.
    pub axis: [f64; 3],
    /// The smallest bond in its layer span (mm²).
    pub bond: f64,
    pub bond_z: f64,
    /// The lighter side's solid (mm³).
    pub carries: f64,
    /// Bigger breaks first.
    pub score: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpotKind {
    /// Joins two masses.
    Neck,
    /// Sticks out of a mass.
    Limb,
    /// A whole thin island.
    Thin,
}

struct Grid {
    origin: [f64; 2],
    c: f64,
    nx: usize,
    ny: usize,
    z: Vec<(f64, f64)>,
    cap: f64,
}

impl Grid {
    fn plane(&self) -> usize { self.nx * self.ny }
    fn point(&self, k: usize, p: usize) -> [f64; 3] {
        [self.origin[0] + (p % self.nx) as f64 * self.c,
         self.origin[1] + (p / self.nx) as f64 * self.c,
         self.z[k].0 - self.z[k].1 * 0.5]
    }
    fn units(&self, mm: f64) -> u16 {
        (mm.min(self.cap) / self.c * 16.0).round() as u16
    }
    fn mm(&self, units: u16) -> f64 { units as f64 * self.c / 16.0 }
    fn neighbors(&self, k: usize, p: usize, mut f: impl FnMut(usize, usize)) {
        let (x, y) = (p % self.nx, p / self.nx);
        for z in k.saturating_sub(1)..=(k + 1).min(self.z.len() - 1) {
            for v in y.saturating_sub(1)..=(y + 1).min(self.ny - 1) {
                for u in x.saturating_sub(1)..=(x + 1).min(self.nx - 1) {
                    if z != k || v != y || u != x { f(z, v * self.nx + u); }
                }
            }
        }
    }
}

fn scan(e: &ExPolygon, grid: &Grid, mut fill: impl FnMut(usize, usize)) {
    let b = e.bbox();
    let lo = ((b.min.y_mm() - grid.origin[1]) / grid.c).floor().max(0.0) as usize;
    let hi = ((((b.max.y_mm() - grid.origin[1]) / grid.c).ceil().max(0.0) as usize) + 1).min(grid.ny);
    let mut crossings = Vec::new();
    for y in lo..hi {
        crossings.clear();
        let py = ((grid.origin[1] + y as f64 * grid.c) * SCALE).round();
        for ring in e.rings() {
            for i in 0..ring.pts.len() {
                let a = ring.pts[i];
                let b = ring.pts[(i + 1) % ring.pts.len()];
                if (a.y as f64 > py) != (b.y as f64 > py) {
                    crossings.push(a.x as f64 + (py - a.y as f64) * (b.x - a.x) as f64 / (b.y - a.y) as f64);
                }
            }
        }
        crossings.sort_unstable_by(f64::total_cmp);
        for pair in crossings.chunks_exact(2) {
            let mut x0 = ((pair[0] / SCALE - grid.origin[0]) / grid.c).floor().max(0.0) as usize;
            let mut x1 = ((((pair[1] / SCALE - grid.origin[0]) / grid.c).ceil().max(0.0) as usize) + 1).min(grid.nx);
            while x0 < x1 && ((grid.origin[0] + x0 as f64 * grid.c) * SCALE).round() < pair[0] { x0 += 1; }
            while x1 > x0 && ((grid.origin[0] + (x1 - 1) as f64 * grid.c) * SCALE).round() >= pair[1] { x1 -= 1; }
            if x0 < x1 { fill(y * grid.nx + x0, y * grid.nx + x1); }
        }
    }
}

// Lower envelope of the squared distance parabolas.
fn edt(line: &[f64], out: &mut [f64], sites: &mut Vec<usize>, cuts: &mut Vec<f64>) {
    sites.clear();
    cuts.clear();
    for q in 0..line.len() {
        let mut cut = f64::NEG_INFINITY;
        while let Some(&p) = sites.last() {
            cut = ((line[q] + (q * q) as f64) - (line[p] + (p * p) as f64)) / (2.0 * (q - p) as f64);
            if cut > *cuts.last().unwrap() { break; }
            sites.pop();
            cuts.pop();
        }
        if sites.is_empty() { cut = f64::NEG_INFINITY; }
        sites.push(q);
        cuts.push(cut);
    }
    let mut site = 0;
    for (q, value) in out.iter_mut().enumerate() {
        while site + 1 < sites.len() && cuts[site + 1] < q as f64 { site += 1; }
        let p = sites[site];
        *value = line[p] + (q as f64 - p as f64).powi(2);
    }
}

fn distance_xy(grid: &Grid, seed: impl Fn(usize) -> bool, stop: &(dyn Fn() -> bool + Sync)) -> Option<Vec<u16>> {
    let cap = (grid.cap / grid.c + 1.0).powi(2);
    let mut square = vec![cap; grid.plane()];
    let (mut sites, mut cuts) = (Vec::new(), Vec::new());
    let mut line = vec![0.0; grid.nx.max(grid.ny)];
    let mut out = line.clone();
    for y in 0..grid.ny {
        if stop() { return None; }
        for x in 0..grid.nx { line[x] = if seed(y * grid.nx + x) { 0.0 } else { cap }; }
        edt(&line[..grid.nx], &mut out[..grid.nx], &mut sites, &mut cuts);
        square[y * grid.nx..(y + 1) * grid.nx].copy_from_slice(&out[..grid.nx]);
    }
    let mut distance = vec![0; grid.plane()];
    for x in 0..grid.nx {
        if stop() { return None; }
        for y in 0..grid.ny { line[y] = square[y * grid.nx + x]; }
        edt(&line[..grid.ny], &mut out[..grid.ny], &mut sites, &mut cuts);
        for y in 0..grid.ny {
            distance[y * grid.nx + x] = grid.units((out[y].sqrt() - 0.5).max(0.0) * grid.c);
        }
    }
    Some(distance)
}

fn distance_stack(grid: &Grid, xy: &[Vec<u16>], outside: bool, stop: &(dyn Fn() -> bool + Sync)) -> Option<Vec<Vec<u16>>> {
    par::try_map(par::Ctl { cancel: Some(stop), ..Default::default() }, xy.len(), |k| {
        let center = grid.z[k].0 - grid.z[k].1 * 0.5;
        let boundary = if outside {
            (center - (grid.z[0].0 - grid.z[0].1)).min(grid.z.last().unwrap().0 - center)
        } else { grid.cap };
        let boundary = (boundary / grid.c * 16.0).powi(2);
        let mut square: Vec<f64> = xy[k].iter().map(|&d| (d as f64).powi(2).min(boundary)).collect();
        let lo = grid.z.partition_point(|&(z, _)| z < center - grid.cap);
        let hi = grid.z.partition_point(|&(z, h)| z - h <= center + grid.cap);
        for j in lo..hi {
            if stop() { return None; }
            if j == k { continue; }
            let dz = if j < k { center - grid.z[j].0 } else { grid.z[j].0 - grid.z[j].1 - center };
            if dz >= grid.cap { continue; }
            let dz2 = (dz.max(0.0) / grid.c * 16.0).powi(2);
            for (value, &d) in square.iter_mut().zip(&xy[j]) {
                *value = value.min((d as f64).powi(2) + dz2);
            }
        }
        Some(square.into_iter().map(|v| v.sqrt().round() as u16).collect())
    }).ok()?.into_iter().collect()
}

#[derive(Clone, Copy)]
struct Run { x0: usize, x1: usize, row: usize, thick: bool, component: usize }

struct Union { parent: Vec<usize>, size: Vec<usize> }

impl Union {
    fn new(n: usize) -> Self { Self { parent: (0..n).collect(), size: vec![1; n] } }
    fn root(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }
    fn join(&mut self, a: usize, b: usize) -> usize {
        let (mut a, mut b) = (self.root(a), self.root(b));
        if a == b { return a; }
        if self.size[a] < self.size[b] { std::mem::swap(&mut a, &mut b); }
        self.parent[b] = a;
        self.size[a] += self.size[b];
        a
    }
}

#[derive(Clone, Default)]
struct Mass { volume: f64, sum: [f64; 3] }

impl Mass {
    fn add(&mut self, p: [f64; 3], volume: f64) {
        self.volume += volume;
        for a in 0..3 { self.sum[a] += p[a] * volume; }
    }
    fn merge(&mut self, m: &Self) {
        self.volume += m.volume;
        for a in 0..3 { self.sum[a] += m.sum[a]; }
    }
    fn center(&self) -> [f64; 3] { self.sum.map(|v| v / self.volume) }
}

struct Component {
    thick: bool,
    runs: Vec<usize>,
    adjacent: BTreeSet<usize>,
    mass: Mass,
    bounds: ([f64; 3], [f64; 3]),
}

struct Components { runs: Vec<Run>, rows: Vec<std::ops::Range<usize>>, parts: Vec<Component> }

impl Components {
    fn at(&self, grid: &Grid, k: usize, p: usize) -> Option<usize> {
        let row = &self.rows[k * grid.ny + p / grid.nx];
        let runs = &self.runs[row.clone()];
        let x = p % grid.nx;
        let i = runs.partition_point(|r| r.x1 <= x);
        runs.get(i).filter(|r| r.x0 <= x).map(|r| r.component)
    }
    fn cells(&self, grid: &Grid, id: usize) -> Vec<(usize, usize)> {
        let mut cells = Vec::new();
        for &r in &self.parts[id].runs {
            let r = self.runs[r];
            for x in r.x0..r.x1 { cells.push((r.row / grid.ny, (r.row % grid.ny) * grid.nx + x)); }
        }
        cells
    }
}

fn components(grid: &Grid, solid: &[Vec<bool>], opening: &[Vec<bool>], stop: &(dyn Fn() -> bool + Sync)) -> Option<Components> {
    let mut runs = Vec::new();
    let mut rows = Vec::new();
    for k in 0..solid.len() {
        if stop() { return None; }
        for y in 0..grid.ny {
            let start = runs.len();
            let mut x = 0;
            while x < grid.nx {
                let p = y * grid.nx + x;
                if !solid[k][p] { x += 1; continue; }
                let x0 = x;
                let thick = opening[k][p];
                x += 1;
                while x < grid.nx && solid[k][y * grid.nx + x] && opening[k][y * grid.nx + x] == thick { x += 1; }
                runs.push(Run { x0, x1: x, row: k * grid.ny + y, thick, component: 0 });
            }
            rows.push(start..runs.len());
        }
    }
    let mut union = Union::new(runs.len());
    let mut links = Vec::new();
    for i in 0..runs.len() {
        if stop() { return None; }
        let r = runs[i];
        let (k, y) = (r.row / grid.ny, r.row % grid.ny);
        let mut connect = |j: usize| {
            let s: Run = runs[j];
            if s.x0 <= r.x1 && r.x0 <= s.x1 {
                if s.thick == r.thick { union.join(i, j); } else { links.push((i, j)); }
            }
        };
        if i > rows[r.row].start { connect(i - 1); }
        if y > 0 { for j in rows[r.row - 1].clone() { connect(j); } }
        if k > 0 {
            for v in y.saturating_sub(1)..=(y + 1).min(grid.ny - 1) {
                for j in rows[(k - 1) * grid.ny + v].clone() { connect(j); }
            }
        }
    }
    let mut ids = vec![usize::MAX; runs.len()];
    let mut kinds = Vec::new();
    for (i, run) in runs.iter_mut().enumerate() {
        let root = union.root(i);
        if ids[root] == usize::MAX {
            ids[root] = kinds.len();
            kinds.push(run.thick);
        }
        run.component = ids[root];
    }
    assemble(grid, runs, rows, &kinds, links, stop)
}

fn assemble(grid: &Grid, runs: Vec<Run>, rows: Vec<std::ops::Range<usize>>, kinds: &[bool], links: Vec<(usize, usize)>, stop: &(dyn Fn() -> bool + Sync)) -> Option<Components> {
    let mut parts: Vec<Component> = kinds.iter().map(|&thick| Component {
        thick, runs: Vec::new(), adjacent: BTreeSet::new(), mass: Mass::default(),
        bounds: ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]),
    }).collect();
    for (i, run) in runs.iter().enumerate() {
        if stop() { return None; }
        let part = &mut parts[run.component];
        part.runs.push(i);
        let (k, y) = (run.row / grid.ny, run.row % grid.ny);
        for x in run.x0..run.x1 {
            let at = grid.point(k, y * grid.nx + x);
            let half = [grid.c * 0.5, grid.c * 0.5, grid.z[k].1 * 0.5];
            part.mass.add(at, grid.c * grid.c * grid.z[k].1);
            for a in 0..3 {
                part.bounds.0[a] = part.bounds.0[a].min(at[a] - half[a]);
                part.bounds.1[a] = part.bounds.1[a].max(at[a] + half[a]);
            }
        }
    }
    for (a, b) in links {
        let (a, b) = (runs[a].component, runs[b].component);
        parts[a].adjacent.insert(b);
        parts[b].adjacent.insert(a);
    }
    Some(Components { runs, rows, parts })
}

fn labeled(grid: &Grid, labels: &[Vec<usize>], kinds: &[bool], stop: &(dyn Fn() -> bool + Sync)) -> Option<Components> {
    let mut runs: Vec<Run> = Vec::new();
    let mut rows: Vec<std::ops::Range<usize>> = Vec::new();
    let mut links = Vec::new();
    for (k, layer) in labels.iter().enumerate() {
        if stop() { return None; }
        for y in 0..grid.ny {
            let start = runs.len();
            let mut x = 0;
            while x < grid.nx {
                let id = layer[y * grid.nx + x];
                if id == usize::MAX { x += 1; continue; }
                let x0 = x;
                while x < grid.nx && layer[y * grid.nx + x] == id { x += 1; }
                let i = runs.len();
                let r = Run { x0, x1: x, row: k * grid.ny + y, thick: kinds[id], component: id };
                let mut connect = |j: usize| {
                    let s = runs[j];
                    if s.thick != r.thick && s.x0 <= r.x1 && r.x0 <= s.x1 { links.push((i, j)); }
                };
                if i > start { connect(i - 1); }
                if y > 0 { for j in rows[r.row - 1].clone() { connect(j); } }
                if k > 0 { for v in y.saturating_sub(1)..=(y + 1).min(grid.ny - 1) {
                    for j in rows[(k - 1) * grid.ny + v].clone() { connect(j); }
                } }
                runs.push(r);
            }
            rows.push(start..runs.len());
        }
    }
    assemble(grid, runs, rows, kinds, links, stop)
}

struct Partition {
    components: Components,
    short: Vec<(usize, (usize, usize), Vec<usize>)>,
}

fn partition(grid: &Grid, solid: &[Vec<bool>], opening: &[Vec<bool>], reach: &[Vec<u16>], distance: &[Vec<u16>], r: f64, stop: &(dyn Fn() -> bool + Sync)) -> Option<Partition> {
    let full = components(grid, solid, opening, stop)?;
    let core: Vec<Vec<bool>> = solid.iter().enumerate().map(|(k, layer)| layer.iter().enumerate()
        .map(|(p, &s)| s && (opening[k][p] || grid.mm(reach[k][p]) > 0.75 * r)).collect()).collect();
    let seeds = components(grid, &core, opening, stop)?;
    let mut kinds: Vec<bool> = seeds.parts.iter().map(|p| p.thick).collect();
    let mut labels = vec![vec![usize::MAX; grid.plane()]; solid.len()];
    for run in &seeds.runs {
        let k = run.row / grid.ny;
        let row = run.row % grid.ny * grid.nx;
        labels[k][row + run.x0..row + run.x1].fill(run.component);
    }
    // An isolated thin solid remains one island, even near another solid.
    for (id, part) in full.parts.iter().enumerate() {
        if !part.thick && part.adjacent.is_empty() {
            let label = kinds.len();
            kinds.push(false);
            for (k, p) in full.cells(grid, id) { labels[k][p] = label; }
        }
    }
    let mut walked = vec![vec![f64::INFINITY; grid.plane()]; solid.len()];
    let mut queue = BinaryHeap::new();
    for k in 0..solid.len() {
        if stop() { return None; }
        for p in 0..grid.plane() {
            let id = labels[k][p];
            if id == usize::MAX || kinds[id] { continue; }
            walked[k][p] = 0.0;
            let mut border = false;
            grid.neighbors(k, p, |z, q| { if solid[z][q] && labels[z][q] == usize::MAX { border = true; } });
            if border { queue.push(Reverse((0u64, id, k, p))); }
        }
    }
    while let Some(Reverse((bits, id, k, p))) = queue.pop() {
        if stop() { return None; }
        let d = f64::from_bits(bits);
        if d != walked[k][p] || labels[k][p] != id { continue; }
        let at = grid.point(k, p);
        grid.neighbors(k, p, |z, q| {
            if !solid[z][q] || opening[z][q] { return; }
            let v = sub(grid.point(z, q), at);
            let next = d + dot(v, v).sqrt();
            if next <= r && next < walked[z][q] {
                walked[z][q] = next;
                labels[z][q] = id;
                queue.push(Reverse((next.to_bits(), id, z, q)));
            }
        });
    }
    drop(walked);
    let grown = labeled(grid, &labels, &kinds, stop)?;
    let mut joined = BTreeSet::new();
    for part in &grown.parts {
        if part.thick { continue; }
        for &a in &part.adjacent { for &b in &part.adjacent { if a < b { joined.insert((a, b)); } } }
    }
    let residual: Vec<Vec<bool>> = solid.iter().enumerate().map(|(k, layer)| layer.iter().enumerate()
        .map(|(p, &s)| s && (opening[k][p] || !core[k][p])).collect()).collect();
    let rest = components(grid, &residual, opening, stop)?;
    let mut short = Vec::new();
    for (id, part) in rest.parts.iter().enumerate() {
        if stop() { return None; }
        if part.thick || part.adjacent.len() < 2 { continue; }
        let mapped: Vec<(usize, usize)> = part.adjacent.iter().map(|&side| {
            let run = rest.runs[rest.parts[side].runs[0]];
            (side, labels[run.row / grid.ny][run.row % grid.ny * grid.nx + run.x0])
        }).collect();
        let cells = rest.cells(grid, id);
        for a in 0..mapped.len() { for b in a + 1..mapped.len() {
            let pair = (mapped[a].1.min(mapped[b].1), mapped[a].1.max(mapped[b].1));
            if joined.contains(&pair) { continue; }
            let at = bottleneck(grid, &rest, &cells, [mapped[a].0, mapped[b].0], distance, stop)?;
            let label = kinds.len();
            kinds.push(false);
            let center = grid.point(at.0, at.1);
            for &(k, p) in &cells {
                let v = sub(grid.point(k, p), center);
                if dot(v, v) <= r * r && labels[k][p] == usize::MAX { labels[k][p] = label; }
            }
            short.push((label, at, vec![pair.0, pair.1]));
            joined.insert(pair);
        } }
    }
    let mut components = labeled(grid, &labels, &kinds, stop)?;
    for (id, _, sides) in &short {
        for &side in sides {
            components.parts[*id].adjacent.insert(side);
            components.parts[side].adjacent.insert(*id);
        }
    }
    Some(Partition { components, short })
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 { (0..3).map(|i| a[i] * b[i]).sum() }
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] { std::array::from_fn(|i| a[i] - b[i]) }

fn axis(grid: &Grid, cells: &[(usize, usize)], center: [f64; 3]) -> [f64; 3] {
    let mut m = [[0.0; 3]; 3];
    for &(k, p) in cells {
        let v = sub(grid.point(k, p), center);
        for i in 0..3 { for j in 0..3 { m[i][j] += v[i] * v[j] * grid.z[k].1; } }
    }
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    loop {
        let (p, q) = [(0, 1), (0, 2), (1, 2)].into_iter().max_by(|&(a, b), &(c, d)| m[a][b].abs().total_cmp(&m[c][d].abs())).unwrap();
        if m[p][q].abs() <= f64::EPSILON * (m[0][0] + m[1][1] + m[2][2]).max(1.0) { break; }
        let theta = 0.5 * (2.0 * m[p][q]).atan2(m[q][q] - m[p][p]);
        let (s, c) = theta.sin_cos();
        for row in &mut m { let (a, b) = (row[p], row[q]); row[p] = c * a - s * b; row[q] = s * a + c * b; }
        for j in 0..3 { let (a, b) = (m[p][j], m[q][j]); m[p][j] = c * a - s * b; m[q][j] = s * a + c * b; }
        m[p][q] = 0.0;
        m[q][p] = 0.0;
        for row in &mut v { let (a, b) = (row[p], row[q]); row[p] = c * a - s * b; row[q] = s * a + c * b; }
    }
    let j = (0..3).max_by(|&a, &b| m[a][a].total_cmp(&m[b][b])).unwrap();
    [v[0][j], v[1][j], v[2][j]]
}

fn bottleneck(grid: &Grid, components: &Components, cells: &[(usize, usize)], sides: [usize; 2], distance: &[Vec<u16>], stop: &(dyn Fn() -> bool + Sync)) -> Option<(usize, usize)> {
    let mut order: Vec<usize> = (0..cells.len()).collect();
    let mut mass = Mass::default();
    for &(k, p) in cells { mass.add(grid.point(k, p), grid.z[k].1); }
    let center = mass.center();
    let separation: Vec<f64> = cells.iter().map(|&(k, p)| { let v = sub(grid.point(k, p), center); dot(v, v) }).collect();
    order.sort_unstable_by(|&a, &b| distance[cells[b].0][cells[b].1].cmp(&distance[cells[a].0][cells[a].1])
        .then(separation[b].total_cmp(&separation[a])).then(a.cmp(&b)));
    let lookup: std::collections::HashMap<(usize, usize), usize> = cells.iter().copied().enumerate().map(|(i, p)| (p, i)).collect();
    let mut union = Union::new(cells.len());
    let mut active = vec![false; cells.len()];
    let mut touches = vec![0u8; cells.len()];
    for i in order {
        if stop() { return None; }
        let (k, p) = cells[i];
        active[i] = true;
        let mut root = i;
        grid.neighbors(k, p, |z, q| {
            if let Some(&j) = lookup.get(&(z, q)) {
                if active[j] {
                    let other = union.root(j);
                    let bits = touches[root] | touches[other];
                    root = union.join(root, other);
                    touches[root] = bits;
                }
            } else if let Some(id) = components.at(grid, z, q) {
                if id == sides[0] { touches[root] |= 1; }
                if id == sides[1] { touches[root] |= 2; }
            }
        });
        if touches[root] == 3 { return Some((k, p)); }
    }
    unreachable!("a connected thin component joins its adjacent sides")
}

fn side_mass(components: &Components, start: usize, removed: &[bool]) -> (Mass, Vec<bool>) {
    let mut visited = vec![false; components.parts.len()];
    let mut queue = vec![start];
    visited[start] = true;
    let mut mass = Mass::default();
    while let Some(id) = queue.pop() {
        mass.merge(&components.parts[id].mass);
        for &next in &components.parts[id].adjacent {
            if !removed[next] && !visited[next] { visited[next] = true; queue.push(next); }
        }
    }
    (mass, visited)
}

// Refine a ridge within its raster cell against the unioned section outlines.
fn refine(grid: &Grid, islands: &[Vec<ExPolygon>], cell: (usize, usize), axis: [f64; 3], stop: &(dyn Fn() -> bool + Sync)) -> Option<([f64; 3], f64)> {
    let start = grid.point(cell.0, cell.1);
    // the widest ball a little round the cell: a ridge cell can be most of a cell off the middle,
    // and a thin place is thin by what that costs
    let reach_layers = ((grid.c / grid.z[cell.0].1).ceil() as isize).max(1);
    let (mut layer, mut at) = (cell.0, [start[0], start[1]]);
    let mut best = ball(grid, islands, layer, at);
    for _ in 0..CLIMBS {
        if stop() { return None; }
        let was = (layer, at);
        let centre = at;
        for y in -2..=2i32 { for x in -2..=2i32 {
            let there = [centre[0] + x as f64 * grid.c * 0.25, centre[1] + y as f64 * grid.c * 0.25];
            let d = ball(grid, islands, layer, there);
            if d > best { (best, at) = (d, there); }
        } }
        // a part lying down has its middle up or down from a ridge cell; one standing up only widens
        let from = if axis[2].abs() < std::f64::consts::FRAC_1_SQRT_2 { layer } else { cell.0 } as isize;
        let lowest = (from - reach_layers).max(0) as usize;
        let highest = ((from + reach_layers) as usize).min(grid.z.len() - 1);
        for k in lowest..=highest {
            let d = ball(grid, islands, k, at);
            if d > best { (best, layer) = (d, k); }
        }
        if (layer, at) == was { break; }
    }
    Some(([at[0], at[1], grid.z[layer].0 - grid.z[layer].1 * 0.5], 2.0 * best))
}

/// The radius of the widest ball at `at` in `layer`'s middle that stays in the part.
fn ball(grid: &Grid, islands: &[Vec<ExPolygon>], layer: usize, at: [f64; 2]) -> f64 {
    let z = grid.z[layer].0 - grid.z[layer].1 * 0.5;
    let lo = grid.z.partition_point(|&(top, _)| top < z - grid.cap);
    let hi = grid.z.partition_point(|&(top, h)| top - h <= z + grid.cap);
    let p = Pt::from_mm(at[0], at[1]);
    let mut d = (z - (grid.z[0].0 - grid.z[0].1)).min(grid.z.last().unwrap().0 - z).min(grid.cap);
    for j in lo..hi {
        let dz = if j < layer { z - grid.z[j].0 } else if j > layer { grid.z[j].0 - grid.z[j].1 - z } else { 0.0 };
        if dz >= d { continue; }
        let mut xy = 0.0;
        if let Some(island) = islands[j].iter().find(|e| e.contains(p)) {
            xy = grid.cap;
            for ring in island.rings() {
                for i in 0..ring.pts.len() {
                    xy = xy.min(dist_point_segment(p, ring.pts[i], ring.pts[(i + 1) % ring.pts.len()]) / SCALE);
                }
            }
        }
        d = d.min((xy * xy + dz.max(0.0).powi(2)).sqrt());
    }
    d
}

/// Where a limb would break first: along it from the tip in, what's beyond each section over that
/// section's strength. A hand on a thin arm loads the arm's root, not the fingers it's thinnest at.
/// The ridge gives each station's thickness and its cells the section's area, so a fin along its
/// whole edge is stronger than a rod as thin.
fn worst_station(grid: &Grid, cells: &[(usize, usize)], ridge: &[(u16, u16, usize, usize)], axis: [f64; 3]) -> Station {
    const STATIONS: usize = 48;
    let along = |k: usize, p: usize| dot(axis, grid.point(k, p));
    let (lo, hi) = cells.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &(k, p)| (lo.min(along(k, p)), hi.max(along(k, p))));
    let station = |k: usize, p: usize| (((along(k, p) - lo) / (hi - lo).max(1e-9) * STATIONS as f64) as usize).min(STATIONS - 1);
    let mut masses = vec![Mass::default(); STATIONS];
    for &(k, p) in cells { masses[station(k, p)].add(grid.point(k, p), grid.c * grid.c * grid.z[k].1); }
    let mut widest: Vec<Option<(u16, usize, usize)>> = vec![None; STATIONS];
    for &(d, _, k, p) in ridge {
        let best = &mut widest[station(k, p)];
        if best.is_none_or(|b| (d, k, p) > b) { *best = Some((d, k, p)); }
    }
    let long = (hi - lo).max(1e-9) / STATIONS as f64;
    let mut worst = (f64::NEG_INFINITY, (ridge[0].2, ridge[0].3), Mass::default(), 1.0);
    let mut beyond = Mass::default();
    for b in (0..STATIONS).rev() {
        beyond.merge(&masses[b]);
        let Some((d, k, p)) = widest[b] else { continue };
        let t = (2.0 * grid.mm(d)).max(grid.c);
        let strength = t * (masses[b].volume / long) / std::f64::consts::FRAC_PI_4;
        let arm = sub(beyond.center(), grid.point(k, p));
        let score = beyond.volume * dot(arm, arm).sqrt().max(t) / strength;
        if score > worst.0 { worst = (score, (k, p), beyond.clone(), strength); }
    }
    // the limb's own thickness: its middle section by width, so fingers on an arm don't speak for it
    let mut sections: Vec<(u16, usize, usize)> = widest.iter().flatten().copied().collect();
    sections.sort_unstable();
    let typical = sections[sections.len() / 2];
    Station { at: worst.1, typical: (typical.1, typical.2), beyond: worst.2, strength: worst.3 }
}

/// [`worst_station`]'s answer.
struct Station {
    /// The section it would break at.
    at: (usize, usize),
    /// A section as thick as the limb mostly is.
    typical: (usize, usize),
    /// What's past `at`.
    beyond: Mass,
    /// `at`'s strength, as a round section's thickness cubed.
    strength: f64,
}

/// How many times [`refine`] moves on to the widest place it found: each goes half a cell.
const CLIMBS: usize = 3;

struct Candidate { id: usize, spot: WeakSpot, sides: Vec<usize> }

/// `(print z, layer height, regions)`, bottom up, and how many times weaker the material is pulled
/// across its layers than along its beads (`layer_factor`: the caller's filament's). Cancellation
/// discards the whole result.
pub fn weak_spots<R: AsRef<[ExPolygon]> + Sync>(layers: &[(f64, f64, R)], min_thickness: f64, layer_factor: f64, stop: &(dyn Fn() -> bool + Sync)) -> Option<Vec<WeakSpot>> {
    if stop() { return None; }
    if layers.is_empty() || !min_thickness.is_finite() || min_thickness <= 0.0 { return Some(Vec::new()); }
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for (_, _, regions) in layers {
        if stop() { return None; }
        for e in regions.as_ref() { for p in &e.contour.pts {
            lo[0] = lo[0].min(p.x_mm()); lo[1] = lo[1].min(p.y_mm());
            hi[0] = hi[0].max(p.x_mm()); hi[1] = hi[1].max(p.y_mm());
        } }
    }
    if !lo[0].is_finite() { return Some(Vec::new()); }
    let r = min_thickness * 0.5;
    let c = (min_thickness / 6.0).min(0.4).max((hi[0] - lo[0]).max(hi[1] - lo[1]) / 256.0);
    let grid = Grid { origin: [lo[0] - c * 0.5, lo[1] - c * 0.5], c,
        nx: ((hi[0] - lo[0]) / c).ceil() as usize + 2,
        ny: ((hi[1] - lo[1]) / c).ceil() as usize + 2,
        z: layers.iter().map(|l| (l.0, l.1)).collect(), cap: r + 2.0 * c };
    let ctl = par::Ctl { cancel: Some(stop), ..Default::default() };
    let islands = par::try_map(ctl, layers.len(), |k| union_ex(layers[k].2.as_ref())).ok()?;
    let solid: Vec<Vec<bool>> = par::try_map(ctl, layers.len(), |k| {
        let mut raster = vec![false; grid.plane()];
        for e in layers[k].2.as_ref() { scan(e, &grid, |a, b| raster[a..b].fill(true)); }
        raster
    }).ok()?;
    let xy: Vec<Vec<u16>> = par::try_map(ctl, layers.len(), |k| distance_xy(&grid, |p| !solid[k][p], stop)).ok()?.into_iter().collect::<Option<_>>()?;
    let distance = distance_stack(&grid, &xy, true, stop)?;
    drop(xy);
    let threshold = r / c * 16.0;
    let xy: Vec<Vec<u16>> = par::try_map(ctl, layers.len(), |k| distance_xy(&grid, |p| distance[k][p] as f64 > threshold, stop)).ok()?.into_iter().collect::<Option<_>>()?;
    let from_eroded = distance_stack(&grid, &xy, false, stop)?;
    drop(xy);
    let opening: Vec<Vec<bool>> = solid.iter().zip(&from_eroded).map(|(s, d)| s.iter().zip(d).map(|(&s, &d)| s && d as f64 <= threshold).collect()).collect();
    drop(from_eroded);
    let xy: Vec<Vec<u16>> = par::try_map(ctl, layers.len(), |k| distance_xy(&grid, |p| opening[k][p], stop)).ok()?.into_iter().collect::<Option<_>>()?;
    let reach = distance_stack(&grid, &xy, false, stop)?;
    drop(xy);
    let Partition { components, short } = partition(&grid, &solid, &opening, &reach, &distance, r, stop)?;
    drop(solid);
    drop(opening);
    let mut candidates = Vec::new();
    for (id, part) in components.parts.iter().enumerate() {
        if stop() { return None; }
        if part.thick || part.runs.is_empty() { continue; }
        let mut sides: Vec<usize> = part.adjacent.iter().copied().collect();
        sides.sort_unstable_by(|&a, &b| components.parts[b].mass.volume.total_cmp(&components.parts[a].mass.volume).then(a.cmp(&b)));
        let kind = match sides.len() {
            0 => SpotKind::Thin,
            1 => SpotKind::Limb,
            _ => SpotKind::Neck,
        };
        let cells = components.cells(&grid, id);
        let mut ridge = Vec::new();
        let (mut at_cell, mut measure_cell) = if kind == SpotKind::Neck {
            let cell = if let Some((_, at, _)) = short.iter().find(|s| s.0 == id) { *at }
                else { bottleneck(&grid, &components, &cells, [sides[0], sides[1]], &distance, stop)? };
            (cell, cell)
        } else {
            for &(k, p) in &cells {
                if stop() { return None; }
                let d = distance[k][p];
                let mut maximum = true;
                grid.neighbors(k, p, |z, q| { if distance[z][q] > d { maximum = false; } });
                if maximum { ridge.push((d, reach[k][p], k, p)); }
            }
            // one that only rises into what it joins has no ridge: its thickest place stands for it
            if ridge.is_empty() {
                ridge.extend(cells.iter().map(|&(k, p)| (distance[k][p], reach[k][p], k, p)).max());
            }
            ridge.sort_unstable();
            let median = ridge[ridge.len() / 2];
            let first = ridge[0];
            ((first.2, first.3), (median.2, median.3))
        };
        let mut axis = axis(&grid, &cells, part.mass.center());
        let mut carries = part.mass.clone();
        let mut strength = None;
        if kind == SpotKind::Limb {
            let heavy = components.parts[sides[0]].mass.center();
            if dot(axis, sub(part.mass.center(), heavy)) < 0.0 { axis = axis.map(|x| -x); }
            let station = worst_station(&grid, &cells, &ridge, axis);
            (at_cell, measure_cell, carries) = (station.at, station.typical, station.beyond);
            strength = Some(station.strength);
        }
        let (measured_at, thickness) = refine(&grid, &islands, measure_cell, axis, stop)?;
        if thickness <= 0.0 { continue; }
        let strength = strength.unwrap_or(thickness.powi(3));
        let at = if at_cell == measure_cell { measured_at } else { refine(&grid, &islands, at_cell, axis, stop)?.0 };
        let lever = dot(sub(carries.center(), at), sub(carries.center(), at)).sqrt().max(thickness);
        let bond_factor = if axis[2].abs() >= std::f64::consts::FRAC_1_SQRT_2 { layer_factor } else { 1.0 };
        candidates.push(Candidate { id, sides, spot: WeakSpot { at, middle: measured_at, thickness, kind, bounds: part.bounds, axis,
            bond: f64::INFINITY, bond_z: grid.z[at_cell.0].0, carries: carries.volume,
            score: if kind == SpotKind::Thin { 0.0 } else { carries.volume * lever / strength * bond_factor } } });
    }
    let mut necks: Vec<(usize, f64)> = candidates.iter().filter(|c| c.spot.kind == SpotKind::Neck)
        .map(|c| (c.id, c.spot.thickness)).collect();
    necks.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    for candidate in &mut candidates {
        if stop() { return None; }
        if candidate.spot.kind != SpotKind::Neck { continue; }
        let mut removed = vec![false; components.parts.len()];
        removed[candidate.id] = true;
        let (mut mass, mut visited) = side_mass(&components, candidate.sides[0], &removed);
        let mut i = 0;
        while visited[candidate.sides[1]] {
            if stop() { return None; }
            let threshold = necks[i].1.max(candidate.spot.thickness);
            while i < necks.len() && necks[i].1 <= threshold {
                removed[necks[i].0] = true;
                i += 1;
            }
            (mass, visited) = side_mass(&components, candidate.sides[0], &removed);
        }
        let (other, opposite) = side_mass(&components, candidate.sides[1], &removed);
        let share = components.parts.iter().enumerate().filter(|(id, part)| removed[*id] && part.adjacent.iter().any(|&a| visited[a]) && part.adjacent.iter().any(|&a| opposite[a])).count();
        let (light, heavy) = if mass.volume <= other.volume { (&mass, &other) } else { (&other, &mass) };
        let spot = &mut candidate.spot;
        if dot(spot.axis, sub(light.center(), heavy.center())) < 0.0 { spot.axis = spot.axis.map(|x| -x); }
        spot.carries = light.volume;
        let offset = sub(light.center(), spot.at);
        let lever = dot(offset, offset).sqrt().max(spot.thickness);
        let factor = if spot.axis[2].abs() >= std::f64::consts::FRAC_1_SQRT_2 { layer_factor } else { 1.0 };
        spot.score = spot.carries * lever / spot.thickness.powi(3) * factor / share as f64;
    }
    let bonds: Vec<Vec<(usize, f64, f64)>> = par::try_map(ctl, layers.len(), |k| {
        let mut result = Vec::new();
        if k == 0 { return result; }
        for island in &islands[k] {
            let mut touched = BTreeSet::new();
            scan(island, &grid, |a, b| { for p in a..b { if let Some(id) = components.at(&grid, k, p) { touched.insert(id); } } });
            let relevant: Vec<usize> = candidates.iter().enumerate().filter(|(_, candidate)| touched.contains(&candidate.id)).map(|(i, _)| i).collect();
            if relevant.is_empty() { continue; }
            let b = island.bbox();
            let below: Vec<ExPolygon> = layers[k - 1].2.as_ref().iter().filter(|e| e.bbox().overlaps(&b)).cloned().collect();
            let area = total_area_mm2(&intersection_ex(std::slice::from_ref(island), &below));
            for i in relevant { result.push((i, area, layers[k].0)); }
        }
        result
    }).ok()?;
    for layer in bonds { for (i, area, z) in layer {
        if area < candidates[i].spot.bond { candidates[i].spot.bond = area; candidates[i].spot.bond_z = z; }
    } }
    if stop() { return None; }
    let mut spots: Vec<WeakSpot> = candidates.into_iter().map(|c| c.spot).filter(|s| s.thickness < min_thickness).collect();
    spots.sort_by(|a, b| b.score.total_cmp(&a.score));
    Some(spots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_fab_slicer::geom::{Polygon, Pt};

    fn rect(x: f64, y: f64, w: f64, h: f64) -> ExPolygon {
        ExPolygon::new(Polygon::new(vec![Pt::from_mm(x, y), Pt::from_mm(x + w, y),
            Pt::from_mm(x + w, y + h), Pt::from_mm(x, y + h)]))
    }

    fn stack(bounds: ([f64; 3], [f64; 3]), solid: impl Fn(f64, f64, f64) -> bool) -> Vec<(f64, f64, Vec<ExPolygon>)> {
        let (lo, hi) = bounds;
        let nx = ((hi[0] - lo[0]) / 0.1).round() as usize;
        let ny = ((hi[1] - lo[1]) / 0.1).round() as usize;
        let nz = ((hi[2] - lo[2]) / 0.2).round() as usize;
        (0..nz).map(|k| {
            let z = lo[2] + (k as f64 + 0.5) * 0.2;
            let mut regions = Vec::new();
            let mut previous = Vec::new();
            let mut first_row = 0;
            for y in 0..=ny {
                let mut runs = Vec::new();
                let mut x = 0;
                if y < ny {
                    while x < nx {
                        if !solid(lo[0] + (x as f64 + 0.5) * 0.1, lo[1] + (y as f64 + 0.5) * 0.1, z) { x += 1; continue; }
                        let start = x;
                        x += 1;
                        while x < nx && solid(lo[0] + (x as f64 + 0.5) * 0.1, lo[1] + (y as f64 + 0.5) * 0.1, z) { x += 1; }
                        runs.push((start, x));
                    }
                }
                if runs != previous {
                    for &(a, b) in &previous { regions.push(rect(lo[0] + a as f64 * 0.1, lo[1] + first_row as f64 * 0.1, (b - a) as f64 * 0.1, (y - first_row) as f64 * 0.1)); }
                    previous = runs;
                    first_row = y;
                }
            }
            (lo[2] + (k + 1) as f64 * 0.2, 0.2, regions)
        }).collect()
    }

    fn dumbbell(x: f64, y: f64, z: f64) -> bool {
        (0.0..30.0).contains(&x) && (0.0..30.0).contains(&y)
            && ((0.0..30.0).contains(&z) || (50.0..80.0).contains(&z)
                || ((12.0..18.0).contains(&x) && (12.0..18.0).contains(&y) && (30.0..50.0).contains(&z)))
    }

    #[test]
    fn strength_dumbbells_in_both_directions() {
        for upright in [true, false] {
            let bounds = if upright { ([0.0; 3], [30.0, 30.0, 80.0]) } else { ([0.0; 3], [80.0, 30.0, 30.0]) };
            let layers = stack(bounds, |x, y, z| if upright { dumbbell(x, y, z) } else { dumbbell(z, y, x) });
            let spots = weak_spots(&layers, 10.0, 1.7, &|| false).unwrap();
            assert_eq!(spots.len(), 1, "upright={upright}: {spots:?}");
            let s = &spots[0];
            assert_eq!(s.kind, SpotKind::Neck);
            assert!((s.thickness - 6.0).abs() <= 0.4, "{s:?}");
            let along = if upright { 2 } else { 0 };
            assert!((30.0..50.0).contains(&s.at[along]), "{s:?}");
            for a in 0..3 { if a != along { assert!((s.at[a] - 15.0).abs() <= 0.4, "{s:?}"); } }
            assert!(s.axis[along].abs() > 0.95, "{s:?}");
            assert!(s.bond.is_finite() && s.bond >= 0.0 && s.carries > 20_000.0 && s.score.is_finite(), "{s:?}");
        }
    }

    #[test]
    fn strength_short_neck() {
        let layers = stack(([0.0; 3], [30.0, 30.0, 63.0]), |x, y, z|
            z < 30.0 || z >= 33.0 || ((12.0..18.0).contains(&x) && (12.0..18.0).contains(&y)));
        let spots = weak_spots(&layers, 10.0, 1.7, &|| false).unwrap();
        assert_eq!(spots.len(), 1, "{spots:?}");
        let s = &spots[0];
        assert_eq!(s.kind, SpotKind::Neck);
        assert!((s.thickness - 6.0).abs() <= 0.4 && (30.0..33.0).contains(&s.at[2]), "{s:?}");
        assert!((s.at[0] - 15.0).abs() <= 0.4 && (s.at[1] - 15.0).abs() <= 0.4, "{s:?}");
    }

    #[test]
    fn strength_blocks_and_small_steps_are_quiet() {
        for step in [false, true] {
            let layers = stack(([0.0; 3], [40.0; 3]), |x, y, z| !step || z < 38.0 || ((2.0..38.0).contains(&x) && (2.0..38.0).contains(&y)));
            let spots = weak_spots(&layers, 10.0, 1.7, &|| false).unwrap();
            assert!(spots.is_empty(), "step={step}: {spots:?}");
        }
    }

    #[test]
    fn strength_neck_ranks_above_a_fin() {
        let layers = stack(([0.0; 3], [50.0, 30.0, 80.0]), |x, y, z| dumbbell(x, y, z)
            || ((30.0..50.0).contains(&x) && (14.2..15.7).contains(&y) && (5.0..25.0).contains(&z)));
        let spots = weak_spots(&layers, 10.0, 1.7, &|| false).unwrap();
        assert_eq!(spots.len(), 2, "{spots:?}");
        assert_eq!(spots[0].kind, SpotKind::Neck, "{spots:?}");
        assert_eq!(spots[1].kind, SpotKind::Limb, "{spots:?}");
        assert!(spots[0].score > spots[1].score && spots[1].at[0] >= 30.0, "{spots:?}");
    }

    #[test]
    fn strength_figure_neck_arms_and_free_legs() {
        let layers = stack(([-32.0, -10.0, 5.0], [32.0, 10.0, 90.0]), |x, y, z| {
            (x.abs() < 12.0 && y.abs() < 8.0 && (30.0..60.0).contains(&z))
                || (x * x + y * y < 9.0 && (60.0..70.0).contains(&z))
                || (x * x + y * y + (z - 80.0).powi(2) < 100.0)
                || ((12.0..32.0).contains(&x.abs()) && y * y + (z - 52.0).powi(2) < 9.0)
                || ((x.abs() - 7.0).powi(2) + y * y < 9.0 && (5.0..30.0).contains(&z))
        });
        let start = std::time::Instant::now();
        let spots = weak_spots(&layers, 12.0, 1.7, &|| false).unwrap();
        eprintln!("strength figure: {:.3} s; thicknesses {:?}", start.elapsed().as_secs_f64(), spots.iter().map(|s| s.thickness).collect::<Vec<_>>());
        assert_eq!(spots.len(), 5, "{spots:?}");
        assert_eq!(spots.iter().filter(|s| s.kind == SpotKind::Neck && (60.0..70.0).contains(&s.at[2])).count(), 1, "{spots:?}");
        for side in [-1.0, 1.0] {
            assert!(spots.iter().any(|s| s.kind == SpotKind::Limb && s.at[0] * side > 12.0 && (s.at[2] - 52.0).abs() < 3.0), "arm {side}: {spots:?}");
            assert!(spots.iter().any(|s| s.kind == SpotKind::Limb && (s.at[0] - side * 7.0).abs() < 3.0 && s.at[2] < 31.0), "leg {side}: {spots:?}");
        }
        assert!(spots.iter().all(|s| (s.kind != SpotKind::Limb || (s.thickness - 6.0).abs() <= 0.4) && s.thickness < 12.0 && s.carries > 0.0 && s.score.is_finite()), "{spots:?}");
    }

    /// A rod lying flat is as thick as it is standing, wherever a layer falls across it.
    #[test]
    fn strength_a_neck_lying_flat_reads_its_thickness() {
        let layers = stack(([-13.0, -10.0, 0.0], [67.0, 82.0, 30.0]), |x, y, z| {
            let (a, b) = (0.6 * x + 0.8 * y, -0.8 * x + 0.6 * y);
            (0.0..90.0).contains(&a) && b.abs() < 15.0 && (!(30.0..60.0).contains(&a) || b * b + (z - 15.0) * (z - 15.0) < 3.2 * 3.2)
        });
        let spots = weak_spots(&layers, 9.0, 1.7, &|| false).unwrap();
        let neck = spots.iter().find(|s| s.kind == SpotKind::Neck).unwrap_or_else(|| panic!("{spots:?}"));
        assert!((neck.thickness - 6.4).abs() < 0.6, "{neck:?}");
    }

    /// A shape whose thin places all rise into something thicker: a horn that narrows steadily, and
    /// fins that flare into the face they stand on. Each still has a thickest place of its own.
    #[test]
    fn strength_tapered_parts_have_a_ridge() {
        for (tip, root) in [(0.5, 4.0), (1.0, 5.0), (2.0, 5.5), (0.3, 2.0)] {
            let layers = stack(([0.0, -15.0, 0.0], [50.0, 15.0, 30.0]), |x, y, z| {
                let r = root + (tip - root) * ((x - 30.0) / 20.0);
                x < 30.0 || y * y + (z - 15.0).powi(2) < r * r
            });
            weak_spots(&layers, 12.0, 1.7, &|| false).unwrap();
            let layers = stack(([0.0, -15.0, 0.0], [40.0, 15.0, 30.0]), |x, y, z| {
                let flare = |d: f64| tip + (root - tip) * (1.0 - d / 10.0).max(0.0).powi(2);
                x < 30.0 || (y.abs() < flare(x - 30.0) && (5.0..25.0).contains(&z)) || (z - 15.0).abs() < flare(x - 30.0) && (5.0..25.0).contains(&(y + 15.0))
            });
            weak_spots(&layers, 12.0, 1.7, &|| false).unwrap();
        }
    }

    #[test]
    fn strength_parallel_legs_share_the_load() {
        let mut scores = Vec::new();
        for two in [false, true] {
            let layers = stack(([-15.0, -15.0, 0.0], [15.0, 15.0, 80.0]), |x, y, z| z < 30.0 || z >= 50.0
                || (y.abs() < 3.0 && ((x + 7.0).abs() < 3.0 || (two && (x - 7.0).abs() < 3.0))));
            let spots = weak_spots(&layers, 10.0, 1.7, &|| false).unwrap();
            assert_eq!(spots.len(), if two { 2 } else { 1 }, "{spots:?}");
            assert!(spots.iter().all(|s| s.kind == SpotKind::Neck && (s.thickness - 6.0).abs() <= 0.4), "{spots:?}");
            scores.push(spots.iter().map(|s| s.score).collect::<Vec<_>>());
        }
        for score in &scores[1] { assert!((scores[0][0] / score - 2.0).abs() < 0.05, "{scores:?}"); }
    }

    #[test]
    fn strength_thin_island_and_cancellation() {
        let layers = stack(([0.0; 3], [30.0, 30.0, 1.0]), |_, _, _| true);
        assert_eq!(weak_spots(&layers, 10.0, 1.7, &|| true), None);
        let spots = weak_spots(&layers, 10.0, 1.7, &|| false).unwrap();
        assert_eq!(spots.len(), 1, "{spots:?}");
        assert_eq!(spots[0].kind, SpotKind::Thin);
        assert!((spots[0].thickness - 1.0).abs() <= 0.2, "{spots:?}");
        assert_eq!(spots[0].score, 0.0);
    }
}
