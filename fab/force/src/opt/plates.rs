//! The plates of a part: where it is a sheet of material, the axis it is thin along and its
//! two faces. A lightening cut goes through a plate (a hole) or into it from one face (a
//! pocket), so the optimiser looks at a part plate by plate.
//!
//! Each solid cell's chord along each grid axis (the run of solid cells through it) gives its
//! thin axis, the shortest. Cells along one thin-axis run make a column; neighbouring columns
//! with the same axis and overlapping runs make a plate. A region as thick as it is wide (a
//! corner, a boss) is no plate.

use crate::grid::{Grid, Lattice};
use crate::V3;

/// A cell counts as solid when at least this share of it is material.
const SOLID: f64 = 0.5;

/// One run of solid cells along the plate's thin axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Column {
    /// The cell indices across the plate (along its `uv` axes).
    pub uv: [usize; 2],
    /// The cells along the thin axis it covers, `start..end`.
    pub span: [usize; 2],
    /// Its faces along the thin axis (mm): where the part's field crosses zero.
    pub faces: [f64; 2],
}

#[derive(Clone, Debug)]
pub struct Plate {
    /// The thin axis (printer frame) and the two axes across it.
    pub axis: usize,
    pub uv: [usize; 2],
    pub columns: Vec<Column>,
    /// The median thickness of its columns (mm).
    pub thickness: f64,
}

impl Plate {
    /// The column at `uv`, if the plate has one there.
    pub fn column(&self, uv: [usize; 2]) -> Option<&Column> {
        self.columns.iter().find(|c| c.uv == uv)
    }

    /// The cell indices across the plate it covers: `lo..hi` along its `uv` axes.
    pub fn extent(&self) -> ([usize; 2], [usize; 2]) {
        let lo = [0, 1].map(|k| self.columns.iter().map(|c| c.uv[k]).min().unwrap_or(0));
        let hi = [0, 1].map(|k| self.columns.iter().map(|c| c.uv[k] + 1).max().unwrap_or(0));
        (lo, hi)
    }
}

/// Each cell's filled share (0..1).
pub fn cell_fill(grid: &Grid) -> Vec<f64> {
    grid.gp.iter().map(|g| g.iter().map(|q| q.fill as f64).sum::<f64>() / (8.0 * 255.0)).collect()
}

/// Per cell and axis: the run of solid cells along that axis that holds it (start, end), or
/// none for a cell that is not solid.
fn runs(grid: &Grid, solid: &[bool]) -> Vec<[Option<[usize; 2]>; 3]> {
    let mut out = vec![[None; 3]; grid.cells()];
    let n = grid.n;
    for a in 0..3 {
        let (b, c) = ((a + 1) % 3, (a + 2) % 3);
        for j in 0..n[b] {
            for k in 0..n[c] {
                let cell = |i: usize| {
                    let mut p = [0; 3];
                    p[a] = i;
                    p[b] = j;
                    p[c] = k;
                    grid.cell(p[0], p[1], p[2])
                };
                let mut i = 0;
                while i < n[a] {
                    if !solid[cell(i)] {
                        i += 1;
                        continue;
                    }
                    let start = i;
                    while i < n[a] && solid[cell(i)] {
                        i += 1;
                    }
                    for t in start..i {
                        out[cell(t)][a] = Some([start, i]);
                    }
                }
            }
        }
    }
    out
}

/// Where the part's field crosses zero along `axis` near `p` (mm along the axis), searched
/// from `from` towards `towards`; the lattice's line nearest `p`, linear between samples.
fn crossing(l: &Lattice, p: V3, axis: usize, from: f64, towards: f64) -> f64 {
    let idx = |a: usize, x: f64| (((x - l.origin[a]) / l.spacing).round().max(0.0) as usize).min(l.n[a] - 1);
    let mut q = [idx(0, p[0]), idx(1, p[1]), idx(2, p[2])];
    let step = if towards > from { 1isize } else { -1 };
    let mut i = idx(axis, from) as isize;
    let end = idx(axis, towards) as isize;
    q[axis] = i as usize;
    let mut prev = l.at(q[0], q[1], q[2]);
    let mut prev_x = l.origin[axis] + i as f64 * l.spacing;
    while i != end {
        i += step;
        q[axis] = i as usize;
        let d = l.at(q[0], q[1], q[2]);
        let x = l.origin[axis] + i as f64 * l.spacing;
        if (prev < 0.0) != (d < 0.0) {
            let t = prev / (prev - d);
            return prev_x + t * (x - prev_x);
        }
        prev = d;
        prev_x = x;
    }
    towards
}

/// The part's plates: neighbouring columns of one thin axis whose runs overlap, at least
/// `min_columns` of them, and at least twice as wide across as they are thick.
pub fn plates(grid: &Grid, lattice: &Lattice, min_columns: usize) -> Vec<Plate> {
    let fill = cell_fill(grid);
    let solid: Vec<bool> = fill.iter().map(|&f| f >= SOLID).collect();
    let runs = runs(grid, &solid);
    // each solid cell's thin axis and its run along it: one column per (axis, uv, span)
    let mut columns: std::collections::BTreeSet<(usize, [usize; 2], [usize; 2])> = Default::default();
    for c in 0..grid.cells() {
        let Some(r) = (0..3).map(|a| runs[c][a]).collect::<Option<Vec<_>>>() else { continue };
        let len = |a: usize| r[a][1] - r[a][0];
        let axis = (0..3).min_by_key(|&a| (len(a), 2 - a)).unwrap();
        let ijk = grid.cell_ijk(c);
        let uv = uv_of(axis);
        columns.insert((axis, [ijk[uv[0]], ijk[uv[1]]], r[axis]));
    }
    let keys: Vec<_> = columns.into_iter().collect();
    let mut by_place: std::collections::HashMap<(usize, [usize; 2]), Vec<usize>> = Default::default();
    for (i, k) in keys.iter().enumerate() {
        by_place.entry((k.0, k.1)).or_default().push(i);
    }
    // union-find over columns that touch across the plate and overlap along it
    let mut parent: Vec<usize> = (0..keys.len()).collect();
    fn root(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for (i, &(axis, uv, span)) in keys.iter().enumerate() {
        for (du, dv) in [(1usize, 0usize), (0, 1)] {
            let next = [uv[0] + du, uv[1] + dv];
            let Some(others) = by_place.get(&(axis, next)) else { continue };
            for &o in others {
                let s = keys[o].2;
                let overlap = span[1].min(s[1]) as isize - span[0].max(s[0]) as isize;
                let shorter = (span[1] - span[0]).min(s[1] - s[0]) as isize;
                if overlap * 2 >= shorter && overlap > 0 {
                    let (a, b) = (root(&mut parent, i), root(&mut parent, o));
                    parent[a] = b;
                }
            }
        }
    }
    let mut groups: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for i in 0..keys.len() {
        let r = root(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let h = grid.h[0];
    let mut out = Vec::new();
    for members in groups.into_values() {
        if members.len() < min_columns {
            continue;
        }
        let axis = keys[members[0]].0;
        let uv = uv_of(axis);
        let mut cols: Vec<Column> = members
            .iter()
            .map(|&m| {
                let (_, cuv, span) = keys[m];
                let mut p = [0.0; 3];
                p[uv[0]] = grid.origin[uv[0]] + (cuv[0] as f64 + 0.5) * h;
                p[uv[1]] = grid.origin[uv[1]] + (cuv[1] as f64 + 0.5) * h;
                let mid = grid.origin[axis] + 0.5 * (span[0] + span[1]) as f64 * h;
                let lo = crossing(lattice, p, axis, mid, grid.origin[axis]);
                let hi = crossing(lattice, p, axis, mid, grid.origin[axis] + grid.n[axis] as f64 * h);
                Column { uv: cuv, span, faces: [lo, hi] }
            })
            .collect();
        cols.sort_by_key(|c| (c.uv[1], c.uv[0]));
        let mut t: Vec<f64> = cols.iter().map(|c| c.faces[1] - c.faces[0]).collect();
        t.sort_by(f64::total_cmp);
        let thickness = t[t.len() / 2];
        let plate = Plate { axis, uv, columns: cols, thickness };
        let (lo, hi) = plate.extent();
        let narrow = (0..2).map(|k| (hi[k] - lo[k]) as f64 * h).fold(f64::INFINITY, f64::min);
        if narrow >= 2.0 * thickness {
            out.push(plate);
        }
    }
    out
}

/// The two axes across a thin axis, in order.
pub fn uv_of(axis: usize) -> [usize; 2] {
    match axis {
        0 => [1, 2],
        1 => [0, 2],
        _ => [0, 1],
    }
}
