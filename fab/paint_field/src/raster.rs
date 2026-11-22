//! A material per cell of a grid on each layer of a stack: a picture's dither as the part prints
//! it. The host describes what makes it ([`RasterSpec`]: the picture and its options) when the
//! script compiles, and makes the grid at build, once it knows the filaments the job loads
//! ([`crate::Program::resolve_rasters`]).
//!
//! The stack runs up the model's y; the grid lies over x and z.

use crate::program::P3;
use std::sync::Arc;

/// What makes a raster, the host's own: equal keys make equal rasters.
pub trait RasterSpec: Send + Sync {
    fn key(&self) -> u64;
    fn as_any(&self) -> &dyn std::any::Any;
}

/// A raster as a mix's field: what makes it and, once made, its grid.
#[derive(Clone)]
pub struct Raster {
    pub spec: Arc<dyn RasterSpec>,
    pub grid: Option<Arc<RasterGrid>>,
}

impl Raster {
    pub fn new(spec: Arc<dyn RasterSpec>) -> Raster {
        Raster { spec, grid: None }
    }
}

impl std::fmt::Debug for Raster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Raster({:016x}, {:?})", self.spec.key(), self.grid.as_ref().map(|g| g.key))
    }
}

impl PartialEq for Raster {
    fn eq(&self, o: &Raster) -> bool {
        self.spec.key() == o.spec.key() && self.grid.as_ref().map(|g| g.key) == o.grid.as_ref().map(|g| g.key)
    }
}

/// Per layer of a stack, a place in the mix's materials for each cell of a grid.
#[derive(Clone, Debug, PartialEq)]
pub struct RasterGrid {
    /// What it holds: equal keys hold the same.
    pub key: u64,
    /// The grid's corner (model mm): its least x, the stack's bottom (y) and its least z.
    pub origin: P3,
    /// The cells' pitch along x and z, and each layer's height (mm).
    pub cell: f64,
    pub layer: f64,
    /// Cells along x and z.
    pub size: [usize; 2],
    /// Per layer from the bottom, per cell (rows along x, from the least z): its place.
    pub places: Vec<Vec<u8>>,
    /// The same: how many cells away (any direction, a diagonal one) the nearest cell of another
    /// place lies, at most 255.
    clear: Vec<Vec<u8>>,
}

impl RasterGrid {
    pub fn new(origin: P3, cell: f64, layer: f64, size: [usize; 2], places: Vec<Vec<u8>>) -> RasterGrid {
        let clear = places.iter().map(|p| clearance(p, size)).collect();
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |b: &[u8]| {
            for &x in b {
                h = (h ^ x as u64).wrapping_mul(0x100_0000_01b3);
            }
        };
        for v in origin.iter().chain([cell, layer].iter()) {
            eat(&v.to_le_bytes());
        }
        eat(&(size[0] as u64).to_le_bytes());
        eat(&(size[1] as u64).to_le_bytes());
        for p in &places {
            eat(p);
        }
        RasterGrid { key: h, origin, cell, layer, size, places, clear }
    }

    /// The layer and cell a point falls in, held to the grid.
    fn locate(&self, p: P3) -> (usize, usize, f64) {
        let held = |v: f64, n: usize| (v.floor().max(0.0) as usize).min(n.saturating_sub(1));
        let (fx, fz) = ((p[0] - self.origin[0]) / self.cell, (p[2] - self.origin[2]) / self.cell);
        let fy = (p[1] - self.origin[1]) / self.layer;
        let (ix, iz, iy) = (held(fx, self.size[0]), held(fz, self.size[1]), held(fy, self.places.len()));
        // how far the point lies inside its cell, toward the nearest side
        let inside = |f: f64, i: usize| (f - i as f64).min(i as f64 + 1.0 - f).max(0.0);
        let edge = (inside(fx, ix) * self.cell).min(inside(fz, iz) * self.cell);
        (iy, iz * self.size[0] + ix, edge)
    }

    /// The place at a point (model mm).
    pub fn place(&self, p: P3) -> f64 {
        let (l, i, _) = self.locate(p);
        self.places[l][i] as f64
    }

    /// A lower bound of the distance (mm) from `p` to where the place can change: the cells
    /// round it that hold the same, past its own cell's side, and no further than the nearest
    /// boundary between two layers.
    pub fn clear_distance(&self, p: P3) -> f64 {
        let (l, i, edge) = self.locate(p);
        let ring = self.clear[l][i].saturating_sub(1) as f64 * self.cell;
        let y = p[1] - self.origin[1];
        let n = self.places.len();
        let to_layer = (1..n).map(|k| (y - k as f64 * self.layer).abs()).fold(f64::INFINITY, f64::min);
        (ring + edge).min(to_layer)
    }
}

/// Per cell, how many cells away (a king's moves) the nearest cell of another value lies.
fn clearance(v: &[u8], size: [usize; 2]) -> Vec<u8> {
    let [nx, nz] = size;
    let mut d = vec![255u8; v.len()];
    let differs = |a: usize, b: usize| v[a] != v[b];
    for z in 0..nz {
        for x in 0..nx {
            let i = z * nx + x;
            let next = |j: usize| if differs(i, j) { 1 } else { 255 };
            let mut best = 255u8;
            for (dx, dz) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)] {
                let (xx, zz) = (x as i64 + dx, z as i64 + dz);
                if xx >= 0 && zz >= 0 && (xx as usize) < nx && (zz as usize) < nz {
                    best = best.min(next(zz as usize * nx + xx as usize));
                }
            }
            d[i] = best;
        }
    }
    // spread the distance out from the edges: forward and back passes of the king's metric
    let pass = |d: &mut Vec<u8>, order: &mut dyn Iterator<Item = (usize, usize)>, steps: &[(i64, i64)]| {
        for (x, z) in order {
            let i = z * nx + x;
            for &(dx, dz) in steps {
                let (xx, zz) = (x as i64 + dx, z as i64 + dz);
                if xx >= 0 && zz >= 0 && (xx as usize) < nx && (zz as usize) < nz {
                    let j = zz as usize * nx + xx as usize;
                    d[i] = d[i].min(d[j].saturating_add(1));
                }
            }
        }
    };
    pass(&mut d, &mut (0..nz).flat_map(|z| (0..nx).map(move |x| (x, z))), &[(-1, 0), (0, -1), (-1, -1), (1, -1)]);
    pass(&mut d, &mut (0..nz).rev().flat_map(|z| (0..nx).rev().map(move |x| (x, z))), &[(1, 0), (0, 1), (1, 1), (-1, 1)]);
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A raster's place at a point is its layer's cell's; the clear distance never passes a cell
    /// of another place, nor the layer's top or bottom, and grows inside a large uniform area.
    #[test]
    fn a_raster_reads_its_cells_and_bounds_its_changes() {
        // two layers of 6 x 4 cells: the bottom all 0 but one cell, the top all 1
        let mut bottom = vec![0u8; 24];
        bottom[2 * 6 + 3] = 2;
        let g = RasterGrid::new([0.0, 0.0, 0.0], 0.5, 0.2, [6, 4], vec![bottom, vec![1; 24]]);
        assert_eq!(g.place([1.75, 0.1, 1.25]), 2.0);
        assert_eq!(g.place([0.1, 0.1, 0.1]), 0.0);
        assert_eq!(g.place([0.1, 0.3, 0.1]), 1.0);
        assert_eq!(g.place([99.0, 99.0, -5.0]), 1.0, "held to the grid");
        let odd = [1.75, 0.1, 1.25];
        for (p, d) in [([0.25, 0.1, 0.25], 0.0), ([1.25, 0.1, 1.25], 0.0)] {
            // the distance never reaches the odd cell or the layer's top
            let bound = g.clear_distance(p);
            let to_cell = ((p[0] - odd[0]).abs() - 0.25).max(0.0).hypot(((p[2] - odd[2]).abs() - 0.25).max(0.0));
            assert!(bound <= to_cell + 1e-9 && bound <= 0.2 - p[1] + 1e-9 && bound >= d, "{p:?}: {bound} vs {to_cell}");
        }
        let top = RasterGrid::new([0.0; 3], 0.5, 0.2, [6, 4], vec![vec![1; 24]]);
        assert!(top.clear_distance([1.5, 0.1, 1.0]) > 0.05, "a uniform layer is clear to its top and bottom");
    }
}
