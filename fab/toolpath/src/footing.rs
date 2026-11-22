//! The broad phase of where a support reaches its footing from.
//!
//! Per layer, a bit per cell of a grid over the part: whether the cell touches what keeps a
//! support off or what it does not reach ([`crate::coral::reach`] holds both, exactly). A
//! cell that touches none of it is reached all over, so most points are answered here, and only
//! a point in a touched cell is asked of the polygons. What is off the grid touches nothing.

use crate::par::{Cancelled, Ctl};
use makepad_fab_slicer::geom::*;

/// The grid's cell (mm).
pub const CELL: f64 = 0.5;

/// The runs of cells (row, first, last) of a grid with its corner and cell `at` (x, y, cell, mm)
/// and `size` cells whose centres `set` covers, by even-odd scanlines at the cell centres.
pub(crate) fn cell_spans(set: &[ExPolygon], (x0, y0, cell): (f64, f64, f64), (nx, ny): (usize, usize)) -> Vec<(usize, usize, usize)> {
    // Each edge's crossings with the cell-centre rows it spans, bucketed by row (even-odd fill
    // between pairs).
    let mut rows: Vec<Vec<f64>> = vec![Vec::new(); ny];
    for e in set {
        for r in e.rings() {
            let m = r.pts.len();
            for i in 0..m {
                let (a, b) = (r.pts[i], r.pts[(i + 1) % m]);
                let (ay, by) = (a.y_mm(), b.y_mm());
                if ay == by {
                    continue;
                }
                let (lo, hi) = (ay.min(by), ay.max(by));
                // Rows whose centre y satisfies lo < y <= hi on the edge's half-open span.
                let j0 = ((lo - y0) / cell - 0.5).floor() as i64 + 1;
                let j1 = ((hi - y0) / cell - 0.5).floor() as i64;
                for jj in j0.max(0)..=j1.min(ny as i64 - 1) {
                    let y = y0 + (jj as f64 + 0.5) * cell;
                    if (ay <= y) != (by <= y) {
                        let t = (y - ay) / (by - ay);
                        rows[jj as usize].push(a.x_mm() + (b.x_mm() - a.x_mm()) * t);
                    }
                }
            }
        }
    }
    let mut spans = Vec::new();
    for (jj, xs) in rows.iter_mut().enumerate() {
        xs.sort_by(f64::total_cmp);
        for pair in xs.chunks(2) {
            if pair.len() < 2 {
                continue;
            }
            let i0 = ((pair[0] - x0) / cell - 0.5).ceil().max(0.0) as usize;
            let i1 = (((pair[1] - x0) / cell - 0.5).floor() as i64).min(nx as i64 - 1);
            if i1 >= i0 as i64 {
                spans.push((jj, i0, i1 as usize));
            }
        }
    }
    spans
}

/// The runs of cells (row, first, last) of a grid laid as in [`cell_spans`] that the edges of
/// `set` pass through or touch: every cell whose square, closed, an edge meets.
fn edge_spans(set: &[ExPolygon], (x0, y0, cell): (f64, f64, f64), (nx, ny): (usize, usize)) -> Vec<(usize, usize, usize)> {
    // A cell's rounding off, so an edge on a cell's side counts for both cells.
    const EPS: f64 = 1e-6;
    let row = |y: f64| ((y - y0) / cell).floor() as i64;
    let col = |x: f64| ((x - x0) / cell).floor() as i64;
    let mut spans = Vec::new();
    for e in set {
        for r in e.rings() {
            let m = r.pts.len();
            for i in 0..m {
                let (a, b) = (r.pts[i], r.pts[(i + 1) % m]);
                let (ax, ay, bx, by) = (a.x_mm(), a.y_mm(), b.x_mm(), b.y_mm());
                let (lo, hi) = (ay.min(by), ay.max(by));
                for j in row(lo - EPS).max(0)..=row(hi + EPS).min(ny as i64 - 1) {
                    // The edge's x within the row's band, its sides included.
                    let (ya, yb) = (y0 + j as f64 * cell - EPS, y0 + (j + 1) as f64 * cell + EPS);
                    let (xa, xb) = if ay == by {
                        (ax.min(bx), ax.max(bx))
                    } else {
                        let at = |y: f64| ax + (bx - ax) * ((y.clamp(lo, hi) - ay) / (by - ay));
                        let (p, q) = (at(ya), at(yb));
                        (p.min(q), p.max(q))
                    };
                    let (i0, i1) = (col(xa - EPS).max(0), col(xb + EPS).min(nx as i64 - 1));
                    if i1 >= i0 {
                        spans.push((j as usize, i0 as usize, i1 as usize));
                    }
                }
            }
        }
    }
    spans
}

/// Per layer, the cells that touch a set (see the module).
pub struct Footing {
    x0: f64,
    y0: f64,
    nx: usize,
    ny: usize,
    /// Words per layer.
    words: usize,
    /// Per layer, a bit per cell: it touches the layer's set.
    bits: Vec<u64>,
}

impl Footing {
    /// The cells each layer's set touches: those its edges meet and those whose centre it covers.
    pub fn cover(ctl: Ctl, sets: &[Vec<ExPolygon>]) -> Result<Self, Cancelled> {
        let n = sets.len();
        let bb = sets.iter().fold(BBox::empty(), |mut b, s| { b.merge(&bbox_of(s)); b });
        if bb.is_empty() {
            return Ok(Footing { x0: 0.0, y0: 0.0, nx: 0, ny: 0, words: 0, bits: Vec::new() });
        }
        // Two cells round it, so the edge cells touch nothing.
        let (x0, y0) = (bb.min.x_mm() - 2.0 * CELL, bb.min.y_mm() - 2.0 * CELL);
        let nx = (((bb.max.x_mm() + 2.0 * CELL - x0) / CELL).ceil() as usize).max(1);
        let ny = (((bb.max.y_mm() + 2.0 * CELL - y0) / CELL).ceil() as usize).max(1);
        let at = (x0, y0, CELL);
        let touched = crate::par::try_map(ctl, n, |l| if sets[l].is_empty() { Vec::new() } else { [cell_spans(&sets[l], at, (nx, ny)), edge_spans(&sets[l], at, (nx, ny))].concat() })?;
        let words = (nx * ny).div_ceil(64);
        let mut bits = vec![0u64; words * n];
        for (l, spans) in touched.iter().enumerate() {
            let word = &mut bits[l * words..(l + 1) * words];
            for &(j, i0, i1) in spans {
                for c in j * nx + i0..=j * nx + i1 {
                    word[c / 64] |= 1 << (c % 64);
                }
            }
        }
        Ok(Footing { x0, y0, nx, ny, words, bits })
    }

    /// Whether the cell `p` lies in on layer `l` touches nothing of the layer's set.
    pub fn clear(&self, p: Pt, l: usize) -> bool {
        let (i, j) = (((p.x_mm() - self.x0) / CELL).floor() as i64, ((p.y_mm() - self.y0) / CELL).floor() as i64);
        if i < 0 || j < 0 || i >= self.nx as i64 || j >= self.ny as i64 || self.words == 0 {
            return true;
        }
        let c = j as usize * self.nx + i as usize;
        self.bits[l * self.words + c / 64] >> (c % 64) & 1 == 0
    }
}
