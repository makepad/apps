//! A part's distance on an even lattice of nodes. It is a distance whatever the field is: near the
//! surface the value over its gradient, past that swept out from there.

use super::{LEAST_CELL, MOST_CELLS, PAST_BOX};
use crate::solid::Solid;
use makepad_csg_sdf::mesh_field::{tri_dist, MeshField};
use makepad_fab_toolpath::par::{self, Cancelled, Ctl};

/// Most nodes in a sim's grid (192 a side): past it the cells grow, so a big part's grid stays a
/// few tens of MB whatever its shape, and a flat or long part's cells are as fine as a round
/// part's of the same volume.
const MOST_NODES: f64 = 192.0 * 192.0 * 192.0;
/// Past this many cells off the surface, the distance comes from the sweep.
const BAND_CELLS: f32 = 2.0;
const LEAST_GRADIENT: f32 = 0.25;
/// How far a triangle's corners may miss a column and still count as crossing it: a crossing too
/// many costs a winding number, one too few gives a whole run the wrong sign.
const CROSSES_WITHIN: f32 = 1e-4;

#[derive(Clone, Debug)]
pub(super) struct Even {
    pub(super) lo: [f32; 3],
    pub(super) cell: f32,
    pub(super) n: [usize; 3],
    d: Vec<f32>,
}

impl Even {
    /// A loose check's grid: [`MOST_CELLS`] along the part's longest side, none finer than
    /// [`LEAST_CELL`].
    pub(super) fn build(ctl: Ctl, solid: &Solid) -> Result<Even, Cancelled> {
        let longest = (0..3).map(|k| solid.hi[k] - solid.lo[k]).fold(0.0, f64::max);
        Self::build_with(ctl, solid, (longest / MOST_CELLS).max(LEAST_CELL))
    }

    /// A sim's grid: [`MOST_NODES`] over the grid's box, cells no finer than `finest` (mm), the
    /// printer's resolution, finer than which a printed part has no shape.
    pub(super) fn build_at(ctl: Ctl, solid: &Solid, finest: f64) -> Result<Even, Cancelled> {
        let volume: f64 = (0..3).map(|k| solid.hi[k] - solid.lo[k] + 2.0 * PAST_BOX).product();
        Self::build_with(ctl, solid, (volume / MOST_NODES).cbrt().max(finest))
    }

    fn build_with(ctl: Ctl, solid: &Solid, cell: f64) -> Result<Even, Cancelled> {
        let size: [f64; 3] = std::array::from_fn(|k| solid.hi[k] - solid.lo[k]);
        let lo: [f64; 3] = std::array::from_fn(|k| solid.lo[k] - PAST_BOX);
        let n: [usize; 3] = std::array::from_fn(|k| ((size[k] + 2.0 * PAST_BOX) / cell).ceil() as usize + 1);
        let d = match &solid.mesh {
            Some(mesh) => distance_from_mesh(ctl, mesh, lo.map(|v| v as f32), cell as f32, n)?,
            None => {
                // a row of nodes at a time: a field's values cost the most, and a worker is never
                // long away from waiting work
                let rows = par::try_map(ctl, n[1] * n[2], |r| {
                    let (y, z) = (lo[1] + (r % n[1]) as f64 * cell, lo[2] + (r / n[1]) as f64 * cell);
                    let at: Vec<[f64; 3]> = (0..n[0]).map(|i| [lo[0] + i as f64 * cell, y, z]).collect();
                    let mut out = vec![0.0; at.len()];
                    solid.values(&at, &mut out);
                    out.into_iter().map(|v| v as f32).collect::<Vec<f32>>()
                })?;
                distance_from_values(ctl, &rows.concat(), n, cell as f32)?
            }
        };
        Ok(Even { lo: lo.map(|v| v as f32), cell: cell as f32, n, d })
    }

    /// The distance at `p` (negative inside) and the unit direction it grows in.
    #[inline]
    pub(super) fn sample(&self, p: [f32; 3]) -> (f32, [f32; 3]) {
        let [nx, ny, nz] = self.n;
        let top = [(nx - 1) as f32 * self.cell, (ny - 1) as f32 * self.cell, (nz - 1) as f32 * self.cell];
        let rel: [f32; 3] = std::array::from_fn(|k| p[k] - self.lo[k]);
        let q: [f32; 3] = std::array::from_fn(|k| rel[k].clamp(0.0, top[k]));
        let u: [f32; 3] = std::array::from_fn(|k| q[k] / self.cell);
        let i = (u[0] as usize).min(nx - 2);
        let j = (u[1] as usize).min(ny - 2);
        let k = (u[2] as usize).min(nz - 2);
        let (tx, ty, tz) = (u[0] - i as f32, u[1] - j as f32, u[2] - k as f32);
        let at = i + nx * (j + ny * k);
        let (sx, sy) = (1, nx);
        let sz = nx * ny;
        let d = &self.d;
        let (d000, d100, d010, d110) = (d[at], d[at + sx], d[at + sy], d[at + sx + sy]);
        let (d001, d101, d011, d111) = (d[at + sz], d[at + sx + sz], d[at + sy + sz], d[at + sx + sy + sz]);
        let (a00, a10, a01, a11) = (d000 + (d100 - d000) * tx, d010 + (d110 - d010) * tx, d001 + (d101 - d001) * tx, d011 + (d111 - d011) * tx);
        let (b0, b1) = (a00 + (a10 - a00) * ty, a01 + (a11 - a01) * ty);
        let value = b0 + (b1 - b0) * tz;
        let off: [f32; 3] = std::array::from_fn(|k| rel[k] - q[k]);
        let far = off[0] * off[0] + off[1] * off[1] + off[2] * off[2];
        if far > 0.0 {
            let far = far.sqrt();
            return (value + far, [off[0] / far, off[1] / far, off[2] / far]);
        }
        let (c00, c10, c01, c11) = (d100 - d000, d110 - d010, d101 - d001, d111 - d011);
        let gx = {
            let (e0, e1) = (c00 + (c10 - c00) * ty, c01 + (c11 - c01) * ty);
            e0 + (e1 - e0) * tz
        };
        let gy = {
            let (e0, e1) = (a10 - a00, a11 - a01);
            e0 + (e1 - e0) * tz
        };
        let gz = b1 - b0;
        let l = (gx * gx + gy * gy + gz * gz).sqrt();
        if l > 1e-6 {
            (value, [gx / l, gy / l, gz / l])
        } else {
            (value, [0.0, 0.0, 1.0])
        }
    }
}

/// A distance from a field's values, on nodes `cell` apart: on the band around the surface the
/// value over its gradient, capped by where the value crosses zero towards a neighbour; off it
/// two chamfer sweeps from the band. The sign is the value's. The band goes a row of nodes at a
/// time across the workers, the sweeps a tile of rows at a time; `ctl` is asked before each.
fn distance_from_values(ctl: Ctl, f: &[f32], n: [usize; 3], cell: f32) -> Result<Vec<f32>, Cancelled> {
    let [nx, ny, nz] = n;
    let (sx, sy, sz) = (1usize, nx, nx * ny);
    let band = BAND_CELLS * cell;
    let rows = par::try_map(ctl, ny * nz, |r| {
        let (j, k) = (r % ny, r / ny);
        let mut d = vec![f32::MAX; nx];
        for i in 0..nx {
            let at = i + nx * (j + ny * k);
            let v = f[at];
            let mut crossing = f32::MAX;
            let mut grad2 = 0.0;
            for (idx, count, step) in [(i, nx, sx), (j, ny, sy), (k, nz, sz)] {
                let before = if idx > 0 { Some(f[at - step]) } else { None };
                let after = if idx + 1 < count { Some(f[at + step]) } else { None };
                for w in [before, after].into_iter().flatten() {
                    if (v < 0.0) != (w < 0.0) {
                        crossing = crossing.min(cell * v.abs() / (v - w).abs().max(1e-9));
                    }
                }
                let g = match (before, after) {
                    (Some(a), Some(b)) => (b - a) / (2.0 * cell),
                    (Some(a), None) => (v - a) / cell,
                    (None, Some(b)) => (b - v) / cell,
                    (None, None) => 0.0,
                };
                grad2 += g * g;
            }
            // A node centred on a thin wall sees no gradient at all, and it is still near.
            let along = v.abs() / grad2.sqrt().max(LEAST_GRADIENT);
            let near = along.min(crossing);
            if near <= band {
                d[i] = near;
            }
        }
        d
    })?;
    let mut d = rows.concat();
    sweep_out(ctl, &mut d, n, cell)?;
    for (v, &value) in d.iter_mut().zip(f) {
        if *v == f32::MAX {
            // No surface anywhere in the grid.
            *v = value.abs();
        }
        if value < 0.0 {
            *v = -*v;
        }
    }
    Ok(d)
}

/// Rows of nodes go to the workers in tiles of this many rows a side (in a sweep's skewed rows).
const TILE: usize = 8;

/// The nodes without a distance get one from those that have: two chamfer sweeps, over the true
/// distance by at most about an eighth. A node takes the least of its own distance and those of
/// the 13 nodes before it, each plus the step to it: the one before it in its row (along x), and
/// three each in the rows (j-1, k), (j-1, k-1), (j, k-1) and (j+1, k-1). Indexed by u = j + k,
/// every row a row reads has a lower u, or the same u and a lower k; so in tiles of TILE x TILE
/// rows over (u, k), a tile reads only itself and tiles on earlier diagonals, and the tiles of
/// one diagonal run side by side across the workers. Every node reads what it would in one pass
/// over the whole grid: the distances are the same to the bit. The second sweep is the first
/// with the grid turned end for end. `ctl` is asked before every tile.
fn sweep_out(ctl: Ctl, d: &mut [f32], n: [usize; 3], cell: f32) -> Result<(), Cancelled> {
    let [nx, ny, nz] = n;
    let (w1, w2, w3) = (cell, cell * std::f32::consts::SQRT_2, cell * 1.732_050_8);
    let nu = ny + nz - 1;
    let (tiles_u, tiles_k) = (nu.div_ceil(TILE), nz.div_ceil(TILE));
    for back in [false, true] {
        // the grid's row (j, k) of the sweep's row (j, k)
        let start = |j: usize, k: usize| if back { nx * ((ny - 1 - j) + ny * (nz - 1 - k)) } else { nx * (j + ny * k) };
        for diagonal in 0..tiles_u + tiles_k - 1 {
            let on: Vec<usize> = (0..tiles_k).filter(|&tk| diagonal >= tk && diagonal - tk < tiles_u).collect();
            let done = {
                let d: &[f32] = d;
                par::try_map(ctl, on.len(), |t| {
                    let (tk, tu) = (on[t], diagonal - on[t]);
                    let ks = tk * TILE..((tk + 1) * TILE).min(nz);
                    let us = tu * TILE..((tu + 1) * TILE).min(nu);
                    let slot = |u: usize, k: usize| ((u - tu * TILE) * TILE + (k - tk * TILE)) * nx;
                    let mut rows = vec![0.0f32; TILE * TILE * nx];
                    for k in ks.clone() {
                        for u in us.clone().filter(|&u| u >= k && u - k < ny) {
                            let (at, s) = (start(u - k, k), slot(u, k));
                            rows[s..s + nx].copy_from_slice(&d[at..at + nx]);
                        }
                    }
                    let mut row = vec![0.0f32; nx];
                    for k in ks.clone() {
                        for u in us.clone().filter(|&u| u >= k && u - k < ny) {
                            let j = u - k;
                            let s = slot(u, k);
                            {
                                // a row before this one: this tile's as swept so far, any other's as an
                                // earlier diagonal left it
                                let before = |j: Option<usize>, k: Option<usize>| -> Option<&[f32]> {
                                    let (j, k) = (j.filter(|&j| j < ny)?, k?);
                                    Some(if us.contains(&(j + k)) && ks.contains(&k) { &rows[slot(j + k, k)..][..nx] } else { &d[start(j, k)..][..nx] })
                                };
                                // each with the steps to its nodes at i and at i -+ 1
                                let near = [
                                    (before(j.checked_sub(1), Some(k)), w1, w2),
                                    (before(j.checked_sub(1), k.checked_sub(1)), w2, w3),
                                    (before(Some(j), k.checked_sub(1)), w1, w2),
                                    (before(Some(j + 1), k.checked_sub(1)), w2, w3),
                                ];
                                row.copy_from_slice(&rows[s..s + nx]);
                                for along in 0..nx {
                                    let i = if back { nx - 1 - along } else { along };
                                    let mut best = row[i];
                                    let mut take = |v: f32, w: f32| {
                                        if v < f32::MAX && v + w < best {
                                            best = v + w;
                                        }
                                    };
                                    if along > 0 {
                                        take(row[if back { i + 1 } else { i - 1 }], w1);
                                    }
                                    for (r, straight, slant) in &near {
                                        if let Some(r) = r {
                                            take(r[i], *straight);
                                            if i > 0 {
                                                take(r[i - 1], *slant);
                                            }
                                            if i + 1 < nx {
                                                take(r[i + 1], *slant);
                                            }
                                        }
                                    }
                                    row[i] = best;
                                }
                            }
                            rows[s..s + nx].copy_from_slice(&row);
                        }
                    }
                    rows
                })?
            };
            for (t, rows) in done.into_iter().enumerate() {
                let (tk, tu) = (on[t], diagonal - on[t]);
                for k in tk * TILE..((tk + 1) * TILE).min(nz) {
                    for u in (tu * TILE..((tu + 1) * TILE).min(nu)).filter(|&u| u >= k && u - k < ny) {
                        let (at, s) = (start(u - k, k), ((u - tu * TILE) * TILE + (k - tk * TILE)) * nx);
                        d[at..at + nx].copy_from_slice(&rows[s..s + nx]);
                    }
                }
            }
        }
    }
    Ok(())
}

/// The nodes `from..=to` along axis `k`.
fn nodes_between(from: f32, to: f32, lo: f32, cell: f32, count: usize) -> std::ops::Range<usize> {
    let first = ((from - lo) / cell).ceil().max(0.0) as usize;
    let last = ((to - lo) / cell).floor();
    if last < 0.0 {
        return 0..0;
    }
    first..(last as usize + 1).min(count)
}

/// A mesh's distance on nodes, drawn from its triangles: each sets the nodes it is near, the rest
/// come from the sweep. Along a column of nodes the sign can only change where a triangle crosses
/// it, so one winding number answers for each run between crossings.
fn distance_from_mesh(ctl: Ctl, mesh: &MeshField, lo: [f32; 3], cell: f32, n: [usize; 3]) -> Result<Vec<f32>, Cancelled> {
    let [nx, ny, nz] = n;
    let band = BAND_CELLS * cell;
    let mut d = vec![f32::MAX; nx * ny * nz];
    let mut crossings: Vec<Vec<f32>> = vec![Vec::new(); nx * ny];
    for (t, tri) in mesh.data.triangles.iter().enumerate() {
        if t % 256 == 0 && ctl.cancelled() {
            return Err(Cancelled);
        }
        let [a, b, c] = tri.map(|i| mesh.data.positions[i as usize]);
        let least: [f32; 3] = std::array::from_fn(|k| a[k].min(b[k]).min(c[k]));
        let most: [f32; 3] = std::array::from_fn(|k| a[k].max(b[k]).max(c[k]));
        let (u, v) = (std::array::from_fn::<f32, 3, _>(|k| b[k] - a[k]), std::array::from_fn::<f32, 3, _>(|k| c[k] - a[k]));
        let normal = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        let size = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
        let plane = size * band;
        for k in nodes_between(least[2] - band, most[2] + band, lo[2], cell, nz) {
            let z = lo[2] + k as f32 * cell;
            for j in nodes_between(least[1] - band, most[1] + band, lo[1], cell, ny) {
                let y = lo[1] + j as f32 * cell;
                for i in nodes_between(least[0] - band, most[0] + band, lo[0], cell, nx) {
                    let x = lo[0] + i as f32 * cell;
                    if (normal[0] * (x - a[0]) + normal[1] * (y - a[1]) + normal[2] * (z - a[2])).abs() > plane {
                        continue;
                    }
                    let near = tri_dist([x, y, z], a, b, c);
                    let at = i + nx * (j + ny * k);
                    if near <= band && near < d[at] {
                        d[at] = near;
                    }
                }
            }
        }
        // Standing on edge, it crosses no column that its neighbours don't.
        if normal[2].abs() > 1e-9 {
            for j in nodes_between(least[1], most[1], lo[1], cell, ny) {
                let y = lo[1] + j as f32 * cell;
                for i in nodes_between(least[0], most[0], lo[0], cell, nx) {
                    let x = lo[0] + i as f32 * cell;
                    let wb = ((x - a[0]) * v[1] - (y - a[1]) * v[0]) / normal[2];
                    let wc = (u[0] * (y - a[1]) - u[1] * (x - a[0])) / normal[2];
                    if wb >= -CROSSES_WITHIN && wc >= -CROSSES_WITHIN && wb + wc <= 1.0 + CROSSES_WITHIN {
                        crossings[i + nx * j].push(a[2] + wb * u[2] + wc * v[2]);
                    }
                }
            }
        }
    }
    for column in &mut crossings {
        column.sort_by(f32::total_cmp);
    }
    let inside = par::try_map(ctl, ny, |j| {
        let mut row = vec![false; nx * nz];
        for i in 0..nx {
            let column = &crossings[i + nx * j];
            if column.is_empty() {
                continue;
            }
            let mut from = 0;
            for end in column.iter().map(|z| (((z - lo[2]) / cell).floor() + 1.0).clamp(0.0, nz as f32) as usize).chain([nz]) {
                if end > from {
                    let middle = lo[2] + ((from + end - 1) / 2) as f32 * cell;
                    if mesh.winding([lo[0] + i as f32 * cell, lo[1] + j as f32 * cell, middle]) > 0.5 {
                        for k in from..end {
                            row[i + nx * k] = true;
                        }
                    }
                    from = end;
                }
            }
        }
        row
    })?;
    sweep_out(ctl, &mut d, n, cell)?;
    for k in 0..nz {
        for (j, row) in inside.iter().enumerate() {
            for i in 0..nx {
                if row[i + nx * k] {
                    let at = i + nx * (j + ny * k);
                    d[at] = -d[at];
                }
            }
        }
    }
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A box as a welded mesh: its grid reads like the box's own distance, in and out.
    #[test]
    fn a_mesh_grid_reads_like_its_distance() {
        let (lo, hi) = ([10.0, 20.0, 0.0], [34.0, 30.0, 16.0]);
        let corner = |c: usize| -> [f64; 3] { std::array::from_fn(|k| if c & (1 << k) == 0 { lo[k] } else { hi[k] }) };
        let positions: Vec<[f64; 3]> = (0..8).map(corner).collect();
        let triangles = vec![[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]];
        let solid = Solid::of_mesh(positions, triangles);
        let grid = Even::build(Ctl::default(), &solid).unwrap();
        for s in 0..4000 {
            let t = s as f64 * 0.618_033_988_75;
            let p: [f64; 3] = std::array::from_fn(|k| lo[k] - 1.5 + (hi[k] - lo[k] + 3.0) * ((t * (k as f64 * 2.0 + 1.0)) % 1.0));
            let q: [f64; 3] = std::array::from_fn(|k| (p[k] - (lo[k] + hi[k]) / 2.0).abs() - (hi[k] - lo[k]) / 2.0);
            let out = (q[0].max(0.0).powi(2) + q[1].max(0.0).powi(2) + q[2].max(0.0).powi(2)).sqrt();
            let exact = out + q[0].max(q[1]).max(q[2]).min(0.0);
            let got = grid.sample(p.map(|v| v as f32)).0 as f64;
            let room = grid.cell as f64 + exact.abs() * 0.15;
            assert!((got - exact).abs() <= room, "at {p:?}: {got} for {exact}");
        }
    }
}
