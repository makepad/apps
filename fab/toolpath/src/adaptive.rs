//! Adaptive and support cubic: cubic infill whose cells subdivide towards
//! the surfaces they hold, like an octree.
//!
//! A cubic family at spacing L contains the families at 2L, 4L, … (every
//! second, fourth, … line). A line whose index k has 2-adic valuation v
//! belongs to the level of spacing L·2^v and to no coarser level; it is
//! printed only where the nearest seed surface is within L·2^v (the cell
//! of that level). Lines of the coarsest level print everywhere. So the
//! spacing grows with the distance from the surfaces, as octree cells do.
//!
//! Seeds: adaptive cubic subdivides towards the solid areas above and
//! below (tops, bottoms, shells); support cubic only towards the solid
//! resting on sparse infill (the undersides of internal tops). Distances
//! come from one 3D Euclidean distance transform over a voxel grid of the
//! object (exact, separable), shared read-only by every layer.

use crate::clipper::{Pieces, FREE};
use crate::fill::{self, Align, RawSeg};
use makepad_fab_slicer::clip::difference_ex;
use makepad_fab_slicer::geom::*;

/// Distance (mm) from each voxel to the nearest seed voxel.
#[derive(Clone, Debug)]
pub struct Field {
    x0: i64,
    y0: i64,
    cell: i64,
    nx: usize,
    ny: usize,
    nz: usize,
    dist: Vec<f32>,
    /// Spacing of the finest level (mm) and the number of levels.
    pub l0: f64,
    pub levels: u32,
}

/// Seeds of the field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seeds {
    /// Every solid area inside the walls (adaptive cubic).
    Solid,
    /// Solid areas resting on sparse infill (support cubic).
    UnderTops,
}

/// Mark the cells whose centres lie in `region` (row scans).
pub(crate) fn raster(region: &[ExPolygon], x0: i64, y0: i64, cell: i64, nx: usize, ny: usize, out: &mut [bool]) {
    if region.is_empty() {
        return;
    }
    // Horizontal lines through the cell centres: y = y0 + (j + ½)·cell.
    let pivot = Pt::new(x0, y0 + cell / 2);
    for sg in fill::family(region, 0.0, unscaled(cell), pivot, Align::Grid) {
        let j = (sg.a.y - y0).div_euclid(cell);
        if j < 0 || j as usize >= ny {
            continue;
        }
        let (xa, xb) = (sg.a.x.min(sg.b.x), sg.a.x.max(sg.b.x));
        // Cells whose centre x0 + (i + ½)·cell lies in [xa, xb].
        let i0 = ((xa - x0) as f64 / cell as f64 - 0.5).ceil().max(0.0) as usize;
        let i1 = (((xb - x0) as f64 / cell as f64 - 0.5).floor() as i64).min(nx as i64 - 1);
        if i1 < i0 as i64 {
            continue;
        }
        for i in i0..=i1 as usize {
            out[j as usize * nx + i] = true;
        }
    }
}

/// 1D squared distance transform with sample spacing `w` (lower envelope
/// of parabolas): f[q] = min_p (w²(q − p)² + f[p]).
pub(crate) fn edt_1d(f: &mut [f32], w: f32, v: &mut Vec<usize>, zz: &mut Vec<f32>, d: &mut Vec<f32>) {
    let n = f.len();
    if n == 0 {
        return;
    }
    let w2 = w * w;
    v.clear();
    zz.clear();
    d.clear();
    d.extend_from_slice(f);
    let inf = f32::INFINITY;
    // Skip leading infinities.
    let first = match (0..n).find(|&q| d[q] < inf) {
        Some(q) => q,
        None => return,
    };
    v.push(first);
    zz.push(-inf);
    zz.push(inf);
    for q in first + 1..n {
        if d[q] == inf {
            continue;
        }
        loop {
            let p = *v.last().unwrap();
            let s = ((d[q] + w2 * (q * q) as f32) - (d[p] + w2 * (p * p) as f32)) / (2.0 * w2 * (q - p) as f32);
            if s <= zz[v.len() - 1] {
                v.pop();
                zz.pop();
                if v.is_empty() {
                    v.push(q);
                    zz.pop();
                    zz.push(-inf);
                    zz.push(inf);
                    break;
                }
            } else {
                v.push(q);
                let last = zz.len() - 1;
                zz[last] = s;
                zz.push(inf);
                break;
            }
        }
    }
    let mut k = 0;
    for (q, out) in f.iter_mut().enumerate() {
        while zz[k + 1] < q as f32 {
            k += 1;
        }
        let p = v[k];
        let dq = q as f32 - p as f32;
        *out = w2 * dq * dq + d[p];
    }
}

impl Field {
    /// The field over an object: `sparse` and `fill` per layer, `h` the
    /// layer heights, `l0` the finest spacing (mm).
    pub fn new(sparse: &[Vec<ExPolygon>], fill: &[Vec<ExPolygon>], h: &[f64], l0: f64, seeds: Seeds) -> Field {
        let nz = sparse.len();
        let mut bb = BBox::empty();
        for l in fill {
            for e in l {
                bb.merge(&e.bbox());
            }
        }
        if bb.is_empty() || nz == 0 {
            return Field { x0: 0, y0: 0, cell: 1, nx: 0, ny: 0, nz: 0, dist: Vec::new(), l0, levels: 1 };
        }
        let (w, d) = (unscaled(bb.max.x - bb.min.x), unscaled(bb.max.y - bb.min.y));
        // About half the finest spacing, capped at 8 M voxels.
        let cell_mm = (l0 / 2.0).max((w * d * nz as f64 / 8.0e6).sqrt()).max(0.2);
        let cell = scaled(cell_mm);
        let nx = (bb.max.x - bb.min.x) as usize / cell as usize + 1;
        let ny = (bb.max.y - bb.min.y) as usize / cell as usize + 1;
        let (x0, y0) = (bb.min.x, bb.min.y);
        let plane = nx * ny;
        let mut dist = vec![f32::INFINITY; plane * nz];
        let mut a = vec![false; plane];
        let mut b = vec![false; plane];
        for i in 0..nz {
            match seeds {
                Seeds::Solid => {
                    let seed = difference_ex(&fill[i], &sparse[i]);
                    a.iter_mut().for_each(|v| *v = false);
                    raster(&seed, x0, y0, cell, nx, ny, &mut a);
                    for (k, v) in a.iter().enumerate() {
                        if *v {
                            dist[i * plane + k] = 0.0;
                        }
                    }
                }
                // The solid of layer i + 1 resting on the sparse area of i:
                // its voxels on layer i seed the field.
                Seeds::UnderTops => {
                    if i + 1 >= nz {
                        continue;
                    }
                    let above = difference_ex(&fill[i + 1], &sparse[i + 1]);
                    a.iter_mut().for_each(|v| *v = false);
                    b.iter_mut().for_each(|v| *v = false);
                    raster(&above, x0, y0, cell, nx, ny, &mut a);
                    raster(&sparse[i], x0, y0, cell, nx, ny, &mut b);
                    for k in 0..plane {
                        if a[k] && b[k] {
                            dist[i * plane + k] = 0.0;
                        }
                    }
                }
            }
        }
        // Separable EDT: x, y, then z (layer spacing: the mean height).
        let hz = (h.iter().sum::<f64>() / h.len().max(1) as f64) as f32;
        let c = cell_mm as f32;
        let (mut v, mut zz, mut tmp) = (Vec::new(), Vec::new(), Vec::new());
        let mut line = Vec::new();
        for i in 0..nz {
            for j in 0..ny {
                let s = &mut dist[i * plane + j * nx..i * plane + (j + 1) * nx];
                edt_1d(s, c, &mut v, &mut zz, &mut tmp);
            }
            for x in 0..nx {
                line.clear();
                line.extend((0..ny).map(|j| dist[i * plane + j * nx + x]));
                edt_1d(&mut line, c, &mut v, &mut zz, &mut tmp);
                for j in 0..ny {
                    dist[i * plane + j * nx + x] = line[j];
                }
            }
        }
        for k in 0..plane {
            line.clear();
            line.extend((0..nz).map(|i| dist[i * plane + k]));
            edt_1d(&mut line, hz, &mut v, &mut zz, &mut tmp);
            for i in 0..nz {
                dist[i * plane + k] = line[i].sqrt();
            }
        }
        // Levels up to cells as large as half the object.
        let mut levels = 1;
        while l0 * (1u64 << levels) as f64 <= w.max(d) / 2.0 && levels < 12 {
            levels += 1;
        }
        Field { x0, y0, cell, nx, ny, nz, dist, l0, levels }
    }

    /// Distance (mm) at a point of layer `li` (infinite outside the grid).
    pub fn at(&self, li: usize, p: Pt) -> f32 {
        if li >= self.nz || self.nx == 0 {
            return f32::INFINITY;
        }
        let i = (p.x - self.x0).div_euclid(self.cell);
        let j = (p.y - self.y0).div_euclid(self.cell);
        if i < 0 || j < 0 || i as usize >= self.nx || j as usize >= self.ny {
            return f32::INFINITY;
        }
        self.dist[li * self.nx * self.ny + j as usize * self.nx + i as usize]
    }

    /// Keep the parts of cubic line segments their level asks for. Lines
    /// of family `fam` sit at k·L0 + `offset` along the normal of `angle`.
    #[allow(clippy::too_many_arguments)]
    pub fn select(&self, li: usize, segs: &[RawSeg], angle: f64, offset: f64, pivot: Pt, out: &mut Pieces) {
        let (sin, cos) = angle.sin_cos();
        let l0 = self.l0;
        let step = self.cell as f64;
        for sg in segs {
            let mid = lerp(sg.a, sg.b, 0.5);
            let v = (-(mid.x - pivot.x) as f64 * sin + (mid.y - pivot.y) as f64 * cos) / SCALE;
            let k = ((v - offset) / l0).round() as i64;
            let val = if k == 0 { 63 } else { k.trailing_zeros() };
            if val + 1 >= self.levels {
                out.push(&[sg.a, sg.b], sg.ea, sg.eb);
                continue;
            }
            let reach = (l0 * (1u64 << val) as f64) as f32;
            // Walk the segment in steps of a cell, keeping runs in reach.
            let len = sg.a.dist(sg.b);
            let n = (len / step).ceil().max(1.0) as usize;
            let mut run_start: Option<usize> = None;
            for t in 0..=n {
                let p = lerp(sg.a, sg.b, t as f64 / n as f64);
                let ok = self.at(li, p) <= reach;
                match (ok, run_start) {
                    (true, None) => run_start = Some(t),
                    (false, Some(t0)) => {
                        self.emit(sg, t0, t - 1, n, out);
                        run_start = None;
                    }
                    _ => {}
                }
            }
            if let Some(t0) = run_start {
                self.emit(sg, t0, n, n, out);
            }
        }
    }

    fn emit(&self, sg: &RawSeg, t0: usize, t1: usize, n: usize, out: &mut Pieces) {
        if t1 <= t0 {
            return;
        }
        let a = if t0 == 0 { sg.a } else { lerp(sg.a, sg.b, t0 as f64 / n as f64) };
        let b = if t1 == n { sg.b } else { lerp(sg.a, sg.b, t1 as f64 / n as f64) };
        out.push(&[a, b], if t0 == 0 { sg.ea } else { FREE }, if t1 == n { sg.eb } else { FREE });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edt_matches_brute_force() {
        let src = [f32::INFINITY, 0.0, f32::INFINITY, f32::INFINITY, f32::INFINITY, 4.0, f32::INFINITY];
        let mut f = src.to_vec();
        edt_1d(&mut f, 0.5, &mut Vec::new(), &mut Vec::new(), &mut Vec::new());
        for q in 0..src.len() {
            let want = (0..src.len()).filter(|&p| src[p].is_finite()).map(|p| 0.25 * ((q as f32 - p as f32).powi(2)) + src[p]).fold(f32::INFINITY, f32::min);
            assert!((f[q] - want).abs() < 1e-4, "{q}: {} vs {want}", f[q]);
        }
    }

    #[test]
    fn field_is_zero_on_seeds_and_grows_away() {
        let sq = |a: f64, b: f64| ExPolygon::new(Polygon::new(vec![Pt::from_mm(a, a), Pt::from_mm(b, a), Pt::from_mm(b, b), Pt::from_mm(a, b)]));
        let n = 50;
        let fill = vec![vec![sq(0.0, 40.0)]; n];
        let mut sparse = fill.clone();
        sparse[n - 1].clear();
        let h = vec![0.2; n];
        let f = Field::new(&sparse, &fill, &h, 4.0, Seeds::Solid);
        let c = Pt::from_mm(20.0, 20.0);
        assert_eq!(f.at(n - 1, c), 0.0);
        assert!((f.at(n - 11, c) - 2.0).abs() < 0.05, "{}", f.at(n - 11, c));
        assert!(f.at(0, c) > f.at(n - 11, c));
        let g = Field::new(&sparse, &fill, &h, 4.0, Seeds::UnderTops);
        assert_eq!(g.at(n - 2, c), 0.0);
    }
}
