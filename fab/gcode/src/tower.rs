//! The prime tower: a block beside the print where the outgoing nozzle rams
//! its filament before a nozzle change and the incoming filament primes
//! after it (R1 §7.6). Every layer up to the last change has a tower layer,
//! so it stands on itself.
//!
//! The tower is split front to back into one block per filament adhesion
//! category, so each material stacks on itself. Inside a block, rows run
//! along X; each row takes its bead width plus a gap (a quarter of the
//! tower bead at the default 150% infill gap): the outgoing filament's
//! ramming rows (wide beads) in its own block, the incoming filament's
//! prime rows in its block. What a block does not use on a layer becomes
//! an empty grid (a frame with ribs across). A wall loop goes round the
//! tower, printed by the tower's wall filament while it is loaded.

use makepad_fab_slicer::clip::{union, FillRule};
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::{offset_ex, Join};

/// One category block: its front inner edge and depth (mm).
#[derive(Clone, Debug)]
pub struct Block {
    pub category: i64,
    pub y0: f64,
    pub depth: f64,
}

#[derive(Clone, Debug)]
pub struct Tower {
    /// Outer lower-left corner and size in mm.
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub d: f64,
    /// Layers 0..=last carry a tower layer.
    pub last_layer: usize,
    /// Bead width of the wall, prime rows and grid.
    pub lw: f64,
    /// Bead width of the ramming rows.
    pub rw: f64,
    /// The gap a row leaves after itself.
    pub gap: f64,
    /// The filament that prints the wall.
    pub wall_filament: usize,
    pub blocks: Vec<Block>,
    /// Ribs along the diagonals: (bar width, extra length). Half the extra runs past each
    /// corner, shrinking with height to none on the top layer.
    pub ribs: Option<(f64, f64)>,
    /// The print heights of its first and last layers.
    pub z0: f64,
    pub top: f64,
}

/// The minimum depth of a tower of a given height (mm), piecewise linear:
/// taller towers need a wider footprint to stand.
pub fn min_depth(height: f64) -> f64 {
    let pts = [(0.0, 5.0), (5.0, 5.0), (100.0, 20.0), (250.0, 40.0), (350.0, 60.0)];
    for w in pts.windows(2) {
        let ((h0, d0), (h1, d1)) = (w[0], w[1]);
        if height <= h1 {
            return d0 + (d1 - d0) * ((height - h0) / (h1 - h0)).clamp(0.0, 1.0);
        }
    }
    60.0
}

/// Cross-section of a tower bead (mm²): the tower prints at full flow.
pub fn bead_area(w: f64, h: f64) -> f64 {
    makepad_fab_toolpath::flow::cross_section(w, h)
}

/// Rows of a bead with cross-section `area` that lay `mm3` on rows
/// `row_len` long.
pub fn row_count(mm3: f64, area: f64, row_len: f64) -> usize {
    if mm3 <= 0.0 || area <= 0.0 || row_len <= 0.0 {
        return 0;
    }
    (mm3 / area / row_len - 1e-9).ceil().max(1.0) as usize
}

/// One section a change lays on a tower layer.
#[derive(Clone, Copy, Debug)]
pub struct Section {
    pub category: i64,
    pub rows: usize,
    pub bead: f64,
}

impl Tower {
    /// Wall centreline rectangle (x0, y0, x1, y1).
    pub fn wall_rect(&self) -> (f64, f64, f64, f64) {
        let h = self.lw / 2.0;
        (self.x + h, self.y + h, self.x + self.w - h, self.y + self.d - h)
    }

    /// The X span of rows of `bead` width inside the wall.
    pub fn row_span(&self, bead: f64) -> (f64, f64) {
        let (x0, _, x1, _) = self.wall_rect();
        let m = self.lw / 2.0 + bead / 2.0;
        (x0 + m, x1 - m)
    }

    pub fn block_of(&self, category: i64) -> usize {
        self.blocks.iter().position(|b| b.category == category).unwrap_or(0)
    }

    /// The tower's outline (outer edge) on the layer printed at `z`: the box, with rib bars
    /// along its diagonals when ribbed, the joins filleted. A bar is at most half the box's
    /// shorter side wide.
    pub fn outline(&self, grow: f64, z: f64) -> Vec<ExPolygon> {
        let (x0, y0, x1, y1) = (self.x - grow, self.y - grow, self.x + self.w + grow, self.y + self.d + grow);
        let mut polys = vec![Polygon::rect(Pt::from_mm(x0, y0), Pt::from_mm(x1, y1))];
        let Some((rib_w, extra)) = self.ribs else {
            return union(&polys, FillRule::NonZero);
        };
        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        for (kx, ky) in [(x1, y1), (x0, y1)] {
            let (dx, dy) = (kx - cx, ky - cy);
            let l = (dx * dx + dy * dy).sqrt();
            let (ux, uy) = (dx / l, dy / l);
            let (nx, ny) = (-uy, ux);
            let past = if self.top > 0.0 { extra / 2.0 * ((self.top - z) / self.top).clamp(0.0, 1.0) } else { 0.0 };
            let (len, hw) = (l + past, rib_w.min(self.w.min(self.d) / 2.0) / 2.0 + grow);
            let c = |a: f64, b: f64| Pt::from_mm(cx + ux * a + nx * b, cy + uy * a + ny * b);
            polys.push(Polygon::new(vec![c(-len, -hw), c(len, -hw), c(len, hw), c(-len, hw)]));
        }
        let u = union(&polys, FillRule::NonZero);
        let round = Join::Round(0.01 * SCALE);
        // Concave joins filleted (closing), then convex ones (opening).
        let closed = offset_ex(&offset_ex(&u, 5.0 * SCALE, round), -5.0 * SCALE, round);
        offset_ex(&offset_ex(&closed, -3.0 * SCALE, round), 3.0 * SCALE, round)
    }

    /// The wall's centreline ring at `z` (counter-clockwise, from the point nearest
    /// the back-right corner).
    fn wall_ring(&self, z: f64) -> Vec<Pt> {
        let o = offset_ex(&self.outline(0.0, z), -self.lw / 2.0 * SCALE, Join::Round(0.01 * SCALE));
        let Some(e) = o.into_iter().max_by(|a, b| a.area2().cmp(&b.area2())) else { return Vec::new() };
        // Fillet points closer than the G-code resolution are noise.
        let mut r = simplify_ring(&e.contour.pts, 0.008 * SCALE);
        if r.len() < 3 {
            return r;
        }
        let (_, _, x1, y1) = self.wall_rect();
        let tr = Pt::from_mm(x1, y1);
        let k = (0..r.len()).min_by_key(|&i| r[i].dist2(tr)).unwrap_or(0);
        r.rotate_left(k);
        r
    }

    /// The wall loop, closed. `skip`: a gap of `len` mm centred where the
    /// left (true) or right side reaches this Y, where the nozzle enters;
    /// the loop then runs in two parts around it.
    pub fn wall(&self, skip: Option<(bool, f64)>, len: f64, z: f64) -> Vec<Vec<Pt>> {
        if self.ribs.is_some() {
            let mut ring = self.wall_ring(z);
            if ring.len() < 3 {
                return Vec::new();
            }
            ring.push(ring[0]);
            let Some((left, sy)) = skip else { return vec![ring] };
            let (x0, _, x1, _) = self.wall_rect();
            let target = Pt::from_mm(if left { x0 } else { x1 }, sy);
            // The arc position of the ring point nearest the entry.
            let mut cum = vec![0.0];
            for w in ring.windows(2) {
                cum.push(cum.last().unwrap() + w[0].dist(w[1]));
            }
            let total = *cum.last().unwrap();
            let mut best = (f64::MAX, 0.0);
            for (i, w) in ring.windows(2).enumerate() {
                let (a, b) = (w[0], w[1]);
                let (abx, aby) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
                let l2 = abx * abx + aby * aby;
                let t = if l2 > 0.0 { (((target.x - a.x) as f64 * abx + (target.y - a.y) as f64 * aby) / l2).clamp(0.0, 1.0) } else { 0.0 };
                let q = Pt::new(a.x + (abx * t) as i64, a.y + (aby * t) as i64);
                let d = q.dist(target);
                if d < best.0 {
                    best = (d, cum[i] + t * l2.sqrt());
                }
            }
            let g = len / 2.0 * SCALE;
            let (s0, s1) = (best.1 - g, best.1 + g);
            if s0 <= 0.0 || s1 >= total {
                return vec![ring];
            }
            return vec![slice_by_length(&ring, &cum, 0.0, s0), slice_by_length(&ring, &cum, s1, total)];
        }
        let (x0, y0, x1, y1) = self.wall_rect();
        let p = |x: f64, y: f64| Pt::from_mm(x, y);
        match skip {
            Some((left, sy)) if sy - len / 2.0 > y0 && sy + len / 2.0 < y1 => {
                let (a, b) = (sy + len / 2.0, sy - len / 2.0);
                if left {
                    vec![vec![p(x1, y1), p(x0, y1), p(x0, a)], vec![p(x0, b), p(x0, y0), p(x1, y0), p(x1, y1)]]
                } else {
                    vec![vec![p(x1, b), p(x1, y0), p(x0, y0), p(x0, y1), p(x1, y1), p(x1, a)]]
                }
            }
            _ => vec![vec![p(x1, y1), p(x0, y1), p(x0, y0), p(x1, y0), p(x1, y1)]],
        }
    }

    /// The empty grid over rows `ya..yb` (centrelines): a frame, and ribs
    /// across it no more than `rib_gap` mm apart.
    pub fn grid(&self, ya: f64, yb: f64, rib_gap: f64) -> (Vec<Pt>, Vec<(Pt, Pt)>) {
        let (gx0, gx1) = self.row_span(self.lw);
        let p = |x: f64, y: f64| Pt::from_mm(x, y);
        let frame = vec![p(gx1, yb), p(gx0, yb), p(gx0, ya), p(gx1, ya), p(gx1, yb)];
        let (rx0, rx1) = (gx0 + 1.0, gx1 - 1.0);
        let n = (((rx1 - rx0) / rib_gap.max(1.0)).ceil() as usize).max(1);
        let mut ribs = Vec::new();
        for k in 0..=n {
            let x = rx0 + (rx1 - rx0) * k as f64 / n as f64;
            let (a, b) = if k % 2 == 0 { (ya, yb) } else { (yb, ya) };
            ribs.push((p(x, a), p(x, b)));
        }
        (frame, ribs)
    }

    pub fn bbox(&self) -> [f64; 4] {
        [self.x, self.y, self.x + self.w, self.y + self.d]
    }
}

/// The depth each category needs on a layer with these sections.
pub fn layer_depths(sections: &[Section], gap: f64) -> Vec<(i64, f64)> {
    let mut out: Vec<(i64, f64)> = Vec::new();
    for s in sections {
        let d = s.rows as f64 * (s.bead + gap);
        match out.iter_mut().find(|o| o.0 == s.category) {
            Some(o) => o.1 += d,
            None => out.push((s.category, d)),
        }
    }
    out
}

/// What is laid on the current tower layer.
#[derive(Clone, Debug)]
pub struct TowerLayer {
    /// Per block: where its next row starts (the front edge of free space).
    pub cursor: Vec<f64>,
    /// Per block: its empty grid is printed.
    pub grid_done: Vec<bool>,
    pub wall_done: bool,
    /// The next rows run left to right.
    pub dir_right: bool,
}

impl TowerLayer {
    pub fn new(t: &Tower) -> Self {
        TowerLayer { cursor: t.blocks.iter().map(|b| b.y0).collect(), grid_done: vec![false; t.blocks.len()], wall_done: false, dir_right: true }
    }
}

/// The speeds (mm/s) of the prime rows after a change, row by row: slow
/// while the new nozzle comes back to temperature, then near full speed.
pub const PRIME_ROW_SPEEDS: [f64; 4] = [29.7, 33.75, 41.2167, 78.75];
pub const PRIME_SPEED: f64 = 79.5833;
/// A climbing family's first four prime rows, as shares of the layer's target speed.
pub const PRIME_ROW_SHARES: [f64; 4] = [0.33, 0.375, 0.458, 0.875];

/// The part of a polyline between arc positions `s0` and `s1` (units).
fn slice_by_length(pts: &[Pt], cum: &[f64], s0: f64, s1: f64) -> Vec<Pt> {
    let at = |s: f64| -> Pt {
        let i = cum.partition_point(|&c| c <= s).clamp(1, pts.len() - 1) - 1;
        let seg = (cum[i + 1] - cum[i]).max(1e-9);
        let t = ((s - cum[i]) / seg).clamp(0.0, 1.0);
        Pt::new(pts[i].x + ((pts[i + 1].x - pts[i].x) as f64 * t) as i64, pts[i].y + ((pts[i + 1].y - pts[i].y) as f64 * t) as i64)
    };
    let mut out = vec![at(s0)];
    for (i, &c) in cum.iter().enumerate() {
        if c > s0 && c < s1 {
            out.push(pts[i]);
        }
    }
    out.push(at(s1));
    out.dedup();
    out
}
