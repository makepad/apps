//! Fitting cuts into the part of a plate that may go: a 2D mask across the plate (cells that
//! may be cut through or into), its clearance (how far each cell is from what must stay),
//! rows of equal holes along its bands and the largest rounded pocket that fits.
//!
//! - Clearance: the exact Euclidean distance transform (Felzenszwalb & Huttenlocher 2012),
//!   two 1D lower-envelope passes.
//! - Bands: the mask's connected pieces with their principal direction (the eigenvector of the
//!   cells' covariance), so a row of holes follows a band the way it runs.
//! - A row: the largest equal holes, spaced at least a web apart and a web from what stays, that
//!   remove the most material net of the wall loops each hole's edge prints (a hole's walls
//!   cost about its perimeter times the wall thickness: holes too small for that are worse than
//!   none).
//! - A pocket: the largest axis-aligned rectangle of the mask (the stack of histograms over its
//!   rows), its corners rounded.

/// A grid of cells across a plate, row-major (`w` along the plate's first axis).
#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    pub w: usize,
    pub h: usize,
    pub on: Vec<bool>,
}

impl Mask {
    pub fn new(w: usize, h: usize) -> Mask {
        Mask { w, h, on: vec![false; w * h] }
    }
    pub fn at(&self, i: usize, j: usize) -> bool {
        self.on[i + self.w * j]
    }
    pub fn set(&mut self, i: usize, j: usize, v: bool) {
        self.on[i + self.w * j] = v;
    }
    pub fn count(&self) -> usize {
        self.on.iter().filter(|v| **v).count()
    }
}

/// The squared distance (in cells) from each cell to the nearest off cell (the border outside
/// counts as off), exact.
pub fn distance2(m: &Mask) -> Vec<f64> {
    const FAR: f64 = 1e20;
    // a frame of off cells round the mask: the border is off
    let (w, h) = (m.w + 2, m.h + 2);
    let mut f = vec![FAR; w * h];
    for j in 0..h {
        for i in 0..w {
            let inside = i >= 1 && j >= 1 && i <= m.w && j <= m.h && m.at(i - 1, j - 1);
            if !inside {
                f[i + w * j] = 0.0;
            }
        }
    }
    let mut line = Vec::new();
    for i in 0..w {
        line.clear();
        line.extend((0..h).map(|j| f[i + w * j]));
        let d = envelope(&line);
        for j in 0..h {
            f[i + w * j] = d[j];
        }
    }
    for j in 0..h {
        let d = envelope(&f[w * j..w * (j + 1)]);
        f[w * j..w * (j + 1)].copy_from_slice(&d);
    }
    let mut out = vec![0.0; m.w * m.h];
    for j in 0..m.h {
        for i in 0..m.w {
            out[i + m.w * j] = f[(i + 1) + w * (j + 1)];
        }
    }
    out
}

/// The 1D distance transform of sampled function `f`: min over q of (p - q)^2 + f(q), by the
/// lower envelope of parabolas.
fn envelope(f: &[f64]) -> Vec<f64> {
    let n = f.len();
    let mut d = vec![0.0; n];
    let mut v = vec![0usize; n];
    let mut z = vec![0.0f64; n + 1];
    let mut k = 0;
    v[0] = 0;
    z[0] = f64::NEG_INFINITY;
    z[1] = f64::INFINITY;
    let cross = |q: usize, p: usize| ((f[q] + (q * q) as f64) - (f[p] + (p * p) as f64)) / (2.0 * q as f64 - 2.0 * p as f64);
    for q in 1..n {
        let mut s = cross(q, v[k]);
        while s <= z[k] {
            k -= 1;
            s = cross(q, v[k]);
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f64::INFINITY;
    }
    k = 0;
    for (q, out) in d.iter_mut().enumerate() {
        while z[k + 1] < q as f64 {
            k += 1;
        }
        let dq = q as f64 - v[k] as f64;
        *out = dq * dq + f[v[k]];
    }
    d
}

/// Per cell: how far (mm) its centre is from the nearest cell that must stay, less half a
/// cell (that cell's edge) and the web; negative for cells too close (or off).
pub fn clearance(m: &Mask, cell: f64, web: f64) -> Vec<f64> {
    distance2(m).iter().zip(&m.on).map(|(d2, on)| if *on { d2.sqrt() * cell - 0.5 * cell - web } else { -1.0 }).collect()
}

/// A connected piece of the cells with clearance of at least `min`, and its run.
#[derive(Clone, Debug)]
pub struct Band {
    pub cells: Vec<usize>,
    /// Its centroid and principal direction (mm across the plate, from cell (0, 0)'s corner).
    pub centre: [f64; 2],
    pub dir: [f64; 2],
}

/// The pieces of cells whose clearance is at least `min`, largest first.
pub fn bands(m: &Mask, clear: &[f64], cell: f64, min: f64) -> Vec<Band> {
    let ok = |i: usize| clear[i] >= min;
    let mut seen = vec![false; clear.len()];
    let mut out = Vec::new();
    for start in 0..clear.len() {
        if seen[start] || !ok(start) {
            continue;
        }
        let mut stack = vec![start];
        seen[start] = true;
        let mut cells = Vec::new();
        while let Some(c) = stack.pop() {
            cells.push(c);
            let (i, j) = (c % m.w, c / m.w);
            let mut push = |ni: usize, nj: usize| {
                let n = ni + m.w * nj;
                if !seen[n] && ok(n) {
                    seen[n] = true;
                    stack.push(n);
                }
            };
            if i > 0 {
                push(i - 1, j);
            }
            if i + 1 < m.w {
                push(i + 1, j);
            }
            if j > 0 {
                push(i, j - 1);
            }
            if j + 1 < m.h {
                push(i, j + 1);
            }
        }
        let pos = |c: usize| [((c % m.w) as f64 + 0.5) * cell, ((c / m.w) as f64 + 0.5) * cell];
        let n = cells.len() as f64;
        let centre = cells.iter().fold([0.0; 2], |a, &c| {
            let p = pos(c);
            [a[0] + p[0] / n, a[1] + p[1] / n]
        });
        let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
        for &c in &cells {
            let p = pos(c);
            let (x, y) = (p[0] - centre[0], p[1] - centre[1]);
            sxx += x * x;
            sxy += x * y;
            syy += y * y;
        }
        // the covariance's larger eigenvector
        let angle = 0.5 * (2.0 * sxy).atan2(sxx - syy);
        out.push(Band { cells, centre, dir: [angle.cos(), angle.sin()] });
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.cells.len()));
    out
}

/// A row of equal round holes across a plate (mm across it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Row {
    pub from: [f64; 2],
    pub to: [f64; 2],
    pub count: usize,
    pub radius: f64,
}

impl Row {
    pub fn centres(&self) -> Vec<[f64; 2]> {
        if self.count == 1 {
            return vec![[0.5 * (self.from[0] + self.to[0]), 0.5 * (self.from[1] + self.to[1])]];
        }
        (0..self.count)
            .map(|i| {
                let t = i as f64 / (self.count - 1) as f64;
                [self.from[0] + t * (self.to[0] - self.from[0]), self.from[1] + t * (self.to[1] - self.from[1])]
            })
            .collect()
    }
}

/// The clearance at a point across the plate (mm): bilinear between the cells' centres, the
/// lowest of the four where one is off (off the mask: -1).
fn clear_at(m: &Mask, clear: &[f64], cell: f64, p: [f64; 2]) -> f64 {
    let (x, y) = (p[0] / cell - 0.5, p[1] / cell - 0.5);
    let (i, j) = (x.floor(), y.floor());
    let (fx, fy) = (x - i, y - j);
    let at = |di: f64, dj: f64| {
        let (a, b) = (i + di, j + dj);
        if a < 0.0 || b < 0.0 || a as usize >= m.w || b as usize >= m.h {
            return -1.0;
        }
        clear[a as usize + m.w * b as usize]
    };
    let c = [at(0.0, 0.0), at(1.0, 0.0), at(0.0, 1.0), at(1.0, 1.0)];
    if c.iter().any(|v| *v < 0.0) {
        return c.iter().copied().fold(f64::INFINITY, f64::min);
    }
    (c[0] * (1.0 - fx) + c[1] * fx) * (1.0 - fy) + (c[2] * (1.0 - fx) + c[3] * fx) * fy
}

/// The row of holes of `radius` along `band`'s middle line: as many as fit a web apart over
/// the longest stretch where the line keeps the radius clear.
pub fn row_of(m: &Mask, clear: &[f64], cell: f64, band: &Band, radius: f64, web: f64) -> Option<Row> {
    let step = 0.25 * cell;
    let reach = (m.w.max(m.h) as f64) * cell;
    let at = |t: f64| [band.centre[0] + t * band.dir[0], band.centre[1] + t * band.dir[1]];
    let mut best: Option<(f64, f64)> = None;
    let mut run: Option<f64> = None;
    let mut t = -reach;
    while t <= reach + step {
        let fits = t <= reach && clear_at(m, clear, cell, at(t)) >= radius;
        match (fits, run) {
            (true, None) => run = Some(t),
            (false, Some(a)) => {
                let b = t - step;
                if best.is_none_or(|(x, y)| b - a > y - x) {
                    best = Some((a, b));
                }
                run = None;
            }
            _ => {}
        }
        t += step;
    }
    let (a, b) = best?;
    let pitch = 2.0 * radius + web;
    let count = ((b - a) / pitch).floor() as usize + 1;
    let (from, to) = if count == 1 { (at(0.5 * (a + b)), at(0.5 * (a + b))) } else { (at(a), at(b)) };
    Some(Row { from, to, count, radius })
}

/// The row along `band` that removes the most material net of its holes' walls: radii from
/// `r_min` to `r_max` in steps of `step`, each hole worth r^2 - 2 r `edge` (`edge`: the
/// equivalent thickness of the wall loops a hole's edge prints, mm), the best kept. A hole
/// takes the room of a circle `grow` times its radius (a teardrop's enclosing circle): the
/// row's radius is that circle's.
pub fn best_row(m: &Mask, clear: &[f64], cell: f64, band: &Band, r_min: f64, r_max: f64, step: f64, web: f64, edge: f64, grow: f64) -> Option<Row> {
    let mut best: Option<(f64, Row)> = None;
    let mut r = r_max;
    while r >= r_min - 1e-9 {
        if let Some(row) = row_of(m, clear, cell, band, r * grow, web) {
            let worth = row.count as f64 * (r * r - 2.0 * r * edge);
            if worth > 0.0 && best.is_none_or(|(w, _)| worth > w) {
                best = Some((worth, row));
            }
        }
        r -= step;
    }
    best.map(|(_, row)| row)
}

/// The largest rectangle of cells with clearance of at least 0 (each a web from what stays):
/// its corner cells, `lo..hi` (exclusive), by area.
pub fn largest_rect(m: &Mask, clear: &[f64]) -> Option<([usize; 2], [usize; 2])> {
    let mut height = vec![0usize; m.w];
    let mut best: Option<(usize, [usize; 2], [usize; 2])> = None;
    for j in 0..m.h {
        for i in 0..m.w {
            height[i] = if clear[i + m.w * j] >= 0.0 { height[i] + 1 } else { 0 };
        }
        // the largest rectangle under the histogram, by a stack of rising bars
        let mut stack: Vec<usize> = Vec::new();
        for i in 0..=m.w {
            let hgt = if i < m.w { height[i] } else { 0 };
            while let Some(&top) = stack.last() {
                if height[top] <= hgt {
                    break;
                }
                stack.pop();
                let left = stack.last().map_or(0, |&s| s + 1);
                let area = height[top] * (i - left);
                if best.is_none_or(|(a, _, _)| area > a) {
                    best = Some((area, [left, j + 1 - height[top]], [i, j + 1]));
                }
            }
            stack.push(i);
        }
    }
    best.filter(|(a, _, _)| *a > 0).map(|(_, lo, hi)| (lo, hi))
}

/// A rectangle (cells `lo..hi`) as mm across the plate: its cells' centres' span (each centre
/// a web from what stays), its centre and size.
pub fn rect_mm(lo: [usize; 2], hi: [usize; 2], cell: f64) -> ([f64; 2], [f64; 2]) {
    let a = [0, 1].map(|k| (lo[k] as f64 + 0.5) * cell);
    let b = [0, 1].map(|k| (hi[k] as f64 - 0.5) * cell);
    ([0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1])], [b[0] - a[0], b[1] - a[1]])
}
