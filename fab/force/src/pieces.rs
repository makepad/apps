//! The pieces of a part's grid: its cells with material, joined where two of them share a
//! face. A piece that holds no fixture has no load path: nothing in it stops it moving as a
//! whole, so its stiffness is singular (six rigid motions free). Neither does one that meets
//! the held part only at an edge or a corner of a cell: two cells sharing an edge keep a
//! hinge there, one corner a ball joint. Both are left out of the solve and reported; a load
//! on one fails the study, as nothing could carry it.
//!
//! The pieces are the connected components of the cells by their faces (6-connectivity),
//! labelled by a flood from each unlabelled cell (Rosenfeld and Pfaltz, "Sequential
//! operations in digital picture processing", Journal of the ACM 13, 1966). Voxel models of
//! micro finite element analysis filter their elements the same way before solving.

use crate::grid::{Gp, Grid};
use crate::V3;

/// What a study leaves out: the pieces nothing holds, their cells and material (mm^3).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Loose {
    pub pieces: usize,
    pub cells: usize,
    pub volume: f64,
}

impl Loose {
    /// "3 loose pieces, 0.42 mm^3".
    pub fn describe(&self) -> String {
        let n = if self.pieces == 1 { "1 loose piece".to_string() } else { format!("{} loose pieces", self.pieces) };
        format!("{n}, {:.2} mm\u{b3}", self.volume)
    }
}

/// Each cell's piece (`u32::MAX`: no material), and how many pieces there are.
pub fn label(grid: &Grid) -> (Vec<u32>, usize) {
    let mut piece = vec![u32::MAX; grid.cells()];
    let mut count = 0u32;
    let mut stack = Vec::new();
    for seed in 0..grid.cells() {
        if piece[seed] != u32::MAX || !grid.active(seed) {
            continue;
        }
        piece[seed] = count;
        stack.push(seed);
        while let Some(c) = stack.pop() {
            let ijk = grid.cell_ijk(c);
            for a in 0..3 {
                for up in [false, true] {
                    let at = if up { ijk[a] + 1 } else { ijk[a].wrapping_sub(1) };
                    if at >= grid.n[a] {
                        continue;
                    }
                    let mut o = ijk;
                    o[a] = at;
                    let o = grid.cell(o[0], o[1], o[2]);
                    if piece[o] == u32::MAX && grid.active(o) {
                        piece[o] = count;
                        stack.push(o);
                    }
                }
            }
        }
        count += 1;
    }
    (piece, count as usize)
}

/// Empties every cell of a piece that holds none of `held`'s nodes. What it emptied, and the
/// share of `force` (its magnitude) on those pieces: nothing could carry it. A piece with less
/// plastic than one cell holds is how a surface fell on the cells, not a piece of the shape, and
/// a surface can pass nodes no cell with material touches: the load on those goes to the rest
/// of the load, so the total stays what was asked.
/// The grid is shared ([`std::sync::Arc`]): it is copied only when a piece is emptied.
pub fn drop_loose(shared: &mut std::sync::Arc<Grid>, held: &[u8], force: &mut [V3]) -> (Loose, f64) {
    let grid: &Grid = shared;
    let (piece, count) = label(grid);
    let cell = grid.h[0] * grid.h[1] * grid.h[2];
    let fill = |grid: &Grid, c: usize| grid.gp[c].iter().map(|g| g.fill as f64 / 255.0).sum::<f64>() * cell / 8.0;
    let (mut kept, mut volume) = (vec![false; count], vec![0.0; count]);
    for c in 0..grid.cells() {
        if piece[c] != u32::MAX {
            let p = piece[c] as usize;
            kept[p] = kept[p] || grid.cell_nodes(c).iter().any(|&n| held[n] != 0);
            volume[p] += fill(grid, c);
        }
    }
    let mut loose = Loose { pieces: kept.iter().filter(|&&k| !k).count(), ..Loose::default() };
    let mut on_piece = vec![false; grid.nodes()];
    for c in 0..grid.cells() {
        if piece[c] != u32::MAX && !kept[piece[c] as usize] {
            loose.cells += 1;
            loose.volume += fill(grid, c);
            if volume[piece[c] as usize] >= cell {
                for n in grid.cell_nodes(c) {
                    on_piece[n] = true;
                }
            }
        }
    }
    if loose.pieces > 0 {
        let grid = std::sync::Arc::make_mut(shared);
        for c in 0..grid.cells() {
            if piece[c] != u32::MAX && !kept[piece[c] as usize] {
                grid.gp[c] = [Gp::default(); 8];
                if let Some(o) = grid.offset.get_mut(c) {
                    *o = [0.0; 3];
                }
            }
        }
    }
    let grid: &Grid = shared;
    let mut touched = vec![false; grid.nodes()];
    for c in 0..grid.cells() {
        if grid.active(c) {
            for n in grid.cell_nodes(c) {
                touched[n] = true;
            }
        }
    }
    let len = |f: &V3| (f[0] * f[0] + f[1] * f[1] + f[2] * f[2]).sqrt();
    let (mut total, mut stranded, mut moved) = (0.0, 0.0, 0.0);
    for (n, f) in force.iter().enumerate() {
        total += len(f);
        if !touched[n] {
            if on_piece[n] { stranded += len(f) } else { moved += len(f) }
        }
    }
    if stranded > 0.0 {
        return (loose, stranded / total);
    }
    if moved > 0.0 {
        if moved >= total {
            return (loose, 1.0);
        }
        let scale = total / (total - moved);
        for (n, f) in force.iter_mut().enumerate() {
            *f = if touched[n] { f.map(|x| x * scale) } else { [0.0; 3] };
        }
    }
    (loose, 0.0)
}
