//! The stress field as the optimiser reads it: each cell's utilisation (how near it is to
//! failing, the failure index's root: 1 / safety factor) and that field smoothed over a
//! radius. The smoothing is the linear "cone" density filter of topology optimisation
//! (Bruns & Tortorelli 2001; Sigmund 2001): a cell's value is the fill-weighted mean of its
//! neighbours', each weighted by how far inside the radius it is. A cut is then decided by the
//! stress around it, not by one lightly loaded cell beside a busy one.

use crate::grid::Grid;
use crate::results::Results;

/// Per cell: 1 / the safety factor of its own reading (0 for an empty or unstressed cell),
/// without the stress recovered at its surface quads ([`Results::cell_sf`]).
pub fn utilisation(r: &Results) -> Vec<f32> {
    r.cells.iter().zip(&r.cell_sf).map(|(c, &sf)| if c[3] > 0.0 && sf.is_finite() && sf > 0.0 { 1.0 / sf } else { 0.0 }).collect()
}

/// `value` (one per cell) smoothed over `radius` mm, weighted by each cell's fill (`fill`, one per
/// cell, 0 for empty: they neither take nor give a value).
pub fn smoothed(grid: &Grid, value: &[f32], fill: &[f32], radius: f64) -> Vec<f32> {
    let h = grid.h[0];
    let reach = (radius / h).floor() as isize;
    let mut out = vec![0.0f32; value.len()];
    if reach < 1 {
        out.copy_from_slice(value);
        return out;
    }
    let n = grid.n.map(|v| v as isize);
    for c in 0..grid.cells() {
        if fill[c] <= 0.0 {
            continue;
        }
        let p = grid.cell_ijk(c).map(|v| v as isize);
        let (mut sum, mut weight) = (0.0f64, 0.0f64);
        for dk in -reach..=reach {
            for dj in -reach..=reach {
                for di in -reach..=reach {
                    let q = [p[0] + di, p[1] + dj, p[2] + dk];
                    if (0..3).any(|a| q[a] < 0 || q[a] >= n[a]) {
                        continue;
                    }
                    let d = ((di * di + dj * dj + dk * dk) as f64).sqrt() * h;
                    let w = radius - d;
                    if w <= 0.0 {
                        continue;
                    }
                    let o = grid.cell(q[0] as usize, q[1] as usize, q[2] as usize);
                    let w = w * fill[o] as f64;
                    sum += w * value[o] as f64;
                    weight += w;
                }
            }
        }
        out[c] = if weight > 0.0 { (sum / weight) as f32 } else { 0.0 };
    }
    out
}

/// Per cell: its mean fill (0..1), as [`Results::cells`] has it.
pub fn fills(r: &Results) -> Vec<f32> {
    r.cells.iter().map(|c| c[3]).collect()
}
