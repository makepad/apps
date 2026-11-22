//! Clipping open polylines to a fill region, fast enough for curve
//! patterns (Hilbert, spirals, honeycombs, gyroid): the region's edges go
//! into a uniform grid once, each polyline segment visits only the cells
//! it crosses. Pieces keep which boundary edge each end lies on, so the
//! chainer can join and anchor them along the walls.
//!
//! Output goes into one flat [`Pieces`] buffer (no allocation per piece).

use makepad_fab_slicer::geom::*;

/// No boundary edge: the piece ends inside the region.
pub const FREE: (u32, u32) = (u32::MAX, u32::MAX);

/// Open polylines in one buffer: piece i is `pts[span[i].0..span[i].1]`,
/// its first point on boundary edge `ea[i]` (ring, edge) and its last on
/// `eb[i]`, or [`FREE`].
#[derive(Clone, Debug, Default)]
pub struct Pieces {
    pub pts: Vec<Pt>,
    pub span: Vec<(u32, u32)>,
    pub ea: Vec<(u32, u32)>,
    pub eb: Vec<(u32, u32)>,
}

impl Pieces {
    pub fn len(&self) -> usize {
        self.span.len()
    }
    pub fn is_empty(&self) -> bool {
        self.span.is_empty()
    }
    pub fn piece(&self, i: usize) -> &[Pt] {
        let (a, b) = self.span[i];
        &self.pts[a as usize..b as usize]
    }
    pub fn first(&self, i: usize) -> Pt {
        self.pts[self.span[i].0 as usize]
    }
    pub fn last(&self, i: usize) -> Pt {
        self.pts[self.span[i].1 as usize - 1]
    }
    /// Append a piece of at least two distinct points.
    pub fn push(&mut self, pts: &[Pt], ea: (u32, u32), eb: (u32, u32)) {
        if pts.len() < 2 {
            return;
        }
        let a = self.pts.len() as u32;
        self.pts.extend_from_slice(pts);
        self.span.push((a, self.pts.len() as u32));
        self.ea.push(ea);
        self.eb.push(eb);
    }
    pub fn append(&mut self, o: &Pieces) {
        for i in 0..o.len() {
            self.push(o.piece(i), o.ea[i], o.eb[i]);
        }
    }
    /// Total length in mm.
    pub fn length_mm(&self) -> f64 {
        (0..self.len()).map(|i| self.piece(i).windows(2).map(|w| w[0].dist(w[1])).sum::<f64>()).sum::<f64>() / SCALE
    }
    pub fn from_segs(segs: &[crate::fill::RawSeg]) -> Pieces {
        let mut p = Pieces { pts: Vec::with_capacity(2 * segs.len()), span: Vec::with_capacity(segs.len()), ea: Vec::with_capacity(segs.len()), eb: Vec::with_capacity(segs.len()) };
        for s in segs {
            if s.a != s.b {
                p.push(&[s.a, s.b], s.ea, s.eb);
            }
        }
        p
    }
}

struct Edge {
    a: Pt,
    b: Pt,
    ring: u32,
    edge: u32,
}

/// A region's boundary edges in a uniform grid.
pub struct Clipper {
    edges: Vec<Edge>,
    x0: i64,
    y0: i64,
    cell: i64,
    nx: usize,
    ny: usize,
    /// CSR: edges of cell c are items[start[c]..start[c + 1]].
    start: Vec<u32>,
    items: Vec<u32>,
    /// Per-segment visit stamps (dedupe an edge met in several cells).
    stamp: Vec<u32>,
    tick: u32,
    pub bbox: BBox,
}

impl Clipper {
    pub fn new(region: &[ExPolygon]) -> Clipper {
        let mut edges = Vec::new();
        let mut bb = BBox::empty();
        for (ri, r) in region.iter().flat_map(|e| e.rings()).enumerate() {
            let n = r.pts.len();
            for i in 0..n {
                let (a, b) = (r.pts[i], r.pts[(i + 1) % n]);
                bb.add(a);
                if a != b {
                    edges.push(Edge { a, b, ring: ri as u32, edge: i as u32 });
                }
            }
        }
        let (w, h) = if bb.is_empty() { (1, 1) } else { ((bb.max.x - bb.min.x).max(1), (bb.max.y - bb.min.y).max(1)) };
        // About two edges per cell, at most 256 cells a side.
        let target = ((edges.len() as f64 / 2.0).sqrt().ceil() as usize).clamp(1, 256);
        let cell = (w.max(h) / target as i64).max(1) + 1;
        let nx = (w / cell + 1) as usize;
        let ny = (h / cell + 1) as usize;
        let mut c = Clipper { edges, x0: bb.min.x, y0: bb.min.y, cell, nx, ny, start: Vec::new(), items: Vec::new(), stamp: Vec::new(), tick: 0, bbox: bb };
        // Two passes: count, then fill.
        let mut count = vec![0u32; nx * ny + 1];
        let mut cells = Vec::new();
        for e in &c.edges {
            cells.clear();
            c.cells_of(e.a, e.b, &mut cells);
            for &k in &cells {
                count[k] += 1;
            }
        }
        let mut start = vec![0u32; nx * ny + 1];
        for k in 0..nx * ny {
            start[k + 1] = start[k] + count[k];
        }
        let mut fill = start.clone();
        let mut items = vec![0u32; start[nx * ny] as usize];
        for (ei, e) in c.edges.iter().enumerate() {
            cells.clear();
            c.cells_of(e.a, e.b, &mut cells);
            for &k in &cells {
                items[fill[k] as usize] = ei as u32;
                fill[k] += 1;
            }
        }
        c.start = start;
        c.items = items;
        c.stamp = vec![0; c.edges.len()];
        c
    }

    fn cx(&self, x: i64) -> i64 {
        (x - self.x0).div_euclid(self.cell)
    }
    fn cy(&self, y: i64) -> i64 {
        (y - self.y0).div_euclid(self.cell)
    }

    /// Grid cells a segment passes through (clamped to the grid; a segment
    /// outside the grid's columns or rows yields nothing).
    fn cells_of(&self, a: Pt, b: Pt, out: &mut Vec<usize>) {
        let (nx, ny) = (self.nx as i64, self.ny as i64);
        let (ca, cb) = (self.cx(a.x), self.cx(b.x));
        let (lo, hi) = (ca.min(cb).max(0), ca.max(cb).min(nx - 1));
        for col in lo..=hi {
            // The segment's y range within this column.
            let xa = self.x0 + col * self.cell;
            let xb = xa + self.cell;
            let (ya, yb) = if a.x == b.x {
                (a.y, b.y)
            } else {
                let t0 = ((xa - a.x) as f64 / (b.x - a.x) as f64).clamp(0.0, 1.0);
                let t1 = ((xb - a.x) as f64 / (b.x - a.x) as f64).clamp(0.0, 1.0);
                let y0 = a.y as f64 + (b.y - a.y) as f64 * t0;
                let y1 = a.y as f64 + (b.y - a.y) as f64 * t1;
                (y0.floor() as i64, y1.ceil() as i64)
            };
            let (r0, r1) = (self.cy(ya.min(yb)).max(0), self.cy(ya.max(yb)).min(ny - 1));
            for row in r0..=r1 {
                out.push(row as usize * self.nx + col as usize);
            }
        }
    }

    /// Point in region by the even-odd rule: a ray to +x through the
    /// point's row of cells, each crossing counted in the cell holding it.
    pub fn contains(&self, p: Pt) -> bool {
        if self.bbox.is_empty() || p.y < self.bbox.min.y || p.y > self.bbox.max.y || p.x > self.bbox.max.x || p.x < self.bbox.min.x {
            return false;
        }
        let row = self.cy(p.y).clamp(0, self.ny as i64 - 1) as usize;
        let col0 = self.cx(p.x).clamp(0, self.nx as i64 - 1) as usize;
        let mut inside = false;
        for col in col0..self.nx {
            let xa = self.x0 + col as i64 * self.cell;
            let xb = xa + self.cell;
            let k = row * self.nx + col;
            for &ei in &self.items[self.start[k] as usize..self.start[k + 1] as usize] {
                let e = &self.edges[ei as usize];
                let (lo, hi) = if e.a.y < e.b.y { (e.a, e.b) } else { (e.b, e.a) };
                // Half-open in y: a vertex on the ray counts once.
                if p.y < lo.y || p.y >= hi.y {
                    continue;
                }
                let x = lo.x as f64 + (hi.x - lo.x) as f64 * ((p.y - lo.y) as f64 / (hi.y - lo.y) as f64);
                let in_cell = x >= xa as f64 && (x < xb as f64 || col == self.nx - 1);
                // The first column also holds crossings left of the cell's
                // start that the clamp moved in; keep only those right of p.
                if x > p.x as f64 && (in_cell || (col == col0 && x < xa as f64)) {
                    inside = !inside;
                }
            }
        }
        inside
    }

    /// The distance (scaled units) from `p` to the region's boundary when it is under `r`, else
    /// `r`: only the grid cells within `r` of `p` are searched.
    pub fn boundary_within(&self, p: Pt, r: i64) -> f64 {
        let mut best = r as f64;
        if self.edges.is_empty() {
            return best;
        }
        let (nx, ny) = (self.nx as i64, self.ny as i64);
        let (c0, c1) = (self.cx(p.x - r).max(0), self.cx(p.x + r).min(nx - 1));
        let (r0, r1) = (self.cy(p.y - r).max(0), self.cy(p.y + r).min(ny - 1));
        for row in r0..=r1 {
            for col in c0..=c1 {
                let k = row as usize * self.nx + col as usize;
                for &ei in &self.items[self.start[k] as usize..self.start[k + 1] as usize] {
                    let e = &self.edges[ei as usize];
                    best = best.min(dist_point_segment(p, e.a, e.b));
                }
            }
        }
        best
    }

    /// Clip an open polyline; the pieces inside go to `out`.
    pub fn clip(&mut self, pl: &[Pt], out: &mut Pieces) {
        if pl.len() < 2 || self.edges.is_empty() {
            return;
        }
        let mut cur: Vec<Pt> = Vec::new();
        let mut cur_ea = FREE;
        let mut inside = self.contains(pl[0]);
        if inside {
            cur.push(pl[0]);
        }
        let mut hits: Vec<(f64, Pt, (u32, u32))> = Vec::new();
        let mut cells = Vec::new();
        for w in pl.windows(2) {
            let (a, b) = (w[0], w[1]);
            if a == b {
                continue;
            }
            // Cheap reject: a segment wholly outside the region's box.
            let bb = &self.bbox;
            if (a.x < bb.min.x && b.x < bb.min.x) || (a.x > bb.max.x && b.x > bb.max.x) || (a.y < bb.min.y && b.y < bb.min.y) || (a.y > bb.max.y && b.y > bb.max.y) {
                if inside {
                    // Cannot happen for a closed boundary; resync.
                    inside = false;
                    if cur.len() >= 2 {
                        out.push(&cur, cur_ea, FREE);
                    }
                    cur.clear();
                }
                continue;
            }
            hits.clear();
            cells.clear();
            self.cells_of(a, b, &mut cells);
            self.tick = self.tick.wrapping_add(1);
            if self.tick == 0 {
                self.stamp.iter_mut().for_each(|s| *s = 0);
                self.tick = 1;
            }
            let d = b.sub(a);
            for &k in &cells {
                for idx in self.start[k]..self.start[k + 1] {
                    let ei = self.items[idx as usize] as usize;
                    if self.stamp[ei] == self.tick {
                        continue;
                    }
                    self.stamp[ei] = self.tick;
                    let e = &self.edges[ei];
                    let f = e.b.sub(e.a);
                    let den = cross(d, f);
                    if den == 0 {
                        continue;
                    }
                    let t = cross(e.a.sub(a), f) as f64 / den as f64;
                    let u = cross(e.a.sub(a), d) as f64 / den as f64;
                    if t > 0.0 && t <= 1.0 && (0.0..1.0).contains(&u) {
                        hits.push((t, lerp(a, b, t), (e.ring, e.edge)));
                    }
                }
            }
            hits.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap());
            // Toggle at each crossing; where crossings bunch (a vertex, a
            // touching edge) decide each stretch by its midpoint instead.
            let ambiguous = hits.windows(2).any(|h| (h[1].0 - h[0].0) * d.x.abs().max(d.y.abs()) as f64 <= 2.0);
            for hi in 0..hits.len() {
                let (t, p, edge) = hits[hi];
                let now = if ambiguous {
                    let t_next = if hi + 1 < hits.len() { hits[hi + 1].0 } else { 1.0 };
                    self.contains(lerp(a, b, (t + t_next) / 2.0))
                } else {
                    !inside
                };
                if now != inside {
                    if inside {
                        cur.push(p);
                        cur.dedup();
                        if cur.len() >= 2 {
                            out.push(&cur, cur_ea, edge);
                        }
                        cur.clear();
                    } else {
                        cur.clear();
                        cur.push(p);
                        cur_ea = edge;
                    }
                    inside = now;
                }
            }
            if inside && (hits.is_empty() || hits.last().unwrap().0 < 1.0) {
                cur.push(b);
            }
        }
        cur.dedup();
        if inside && cur.len() >= 2 {
            out.push(&cur, cur_ea, FREE);
        }
    }
}

/// Join segments that meet end to end into polylines: at a point where two
/// segments meet they continue each other; where more meet, the straightest
/// pairs continue and the rest end there. Order follows the input order.
pub fn link(segs: &[(Pt, Pt)]) -> Vec<Vec<Pt>> {
    let n = segs.len();
    // Ends sorted by point: end k is segment k/2, side k%2.
    let end = |k: usize| if k % 2 == 0 { segs[k / 2].0 } else { segs[k / 2].1 };
    let mut order: Vec<usize> = (0..2 * n).collect();
    order.sort_by_key(|&k| (end(k).x, end(k).y, k));
    let mut next: Vec<usize> = vec![usize::MAX; 2 * n];
    let mut i = 0;
    while i < order.len() {
        let mut j = i;
        while j < order.len() && end(order[j]) == end(order[i]) {
            j += 1;
        }
        let group = &order[i..j];
        if group.len() >= 2 {
            // Direction leaving the point along each segment.
            let dir = |k: usize| {
                let o = end(k ^ 1);
                let p = end(k);
                ((o.y - p.y) as f64).atan2((o.x - p.x) as f64)
            };
            let mut free: Vec<usize> = group.to_vec();
            while free.len() >= 2 {
                // The straightest pair: directions closest to opposite.
                let mut best = (f64::INFINITY, 0, 1);
                for a in 0..free.len() {
                    for b in a + 1..free.len() {
                        let mut dd = (dir(free[a]) - dir(free[b])).abs();
                        if dd > std::f64::consts::PI {
                            dd = std::f64::consts::TAU - dd;
                        }
                        let bend = std::f64::consts::PI - dd;
                        if bend < best.0 - 1e-9 {
                            best = (bend, a, b);
                        }
                    }
                }
                let (ka, kb) = (free[best.1], free[best.2]);
                if ka / 2 != kb / 2 {
                    next[ka] = kb;
                    next[kb] = ka;
                }
                free.remove(best.2);
                free.remove(best.1);
            }
        }
        i = j;
    }
    let mut used = vec![false; n];
    let mut out = Vec::new();
    let walk = |start: usize, used: &mut Vec<bool>| -> Vec<Pt> {
        let mut pts = vec![end(start)];
        let mut k = start;
        loop {
            used[k / 2] = true;
            let other = k ^ 1;
            pts.push(end(other));
            let nk = next[other];
            if nk == usize::MAX || used[nk / 2] {
                break;
            }
            k = nk;
        }
        pts
    };
    // Open chains from their free ends first, then closed loops.
    for s in 0..n {
        for k in [2 * s, 2 * s + 1] {
            if !used[s] && next[k] == usize::MAX {
                out.push(walk(k, &mut used));
            }
        }
    }
    for s in 0..n {
        if !used[s] {
            out.push(walk(2 * s, &mut used));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> ExPolygon {
        ExPolygon::new(Polygon::new(vec![Pt::from_mm(x0, y0), Pt::from_mm(x1, y0), Pt::from_mm(x1, y1), Pt::from_mm(x0, y1)]))
    }

    #[test]
    fn clip_through_square_with_hole() {
        let mut e = square(0.0, 0.0, 10.0, 10.0);
        let mut hole = Polygon::new(vec![Pt::from_mm(4.0, 4.0), Pt::from_mm(4.0, 6.0), Pt::from_mm(6.0, 6.0), Pt::from_mm(6.0, 4.0)]);
        if hole.is_ccw() {
            hole.reverse();
        }
        e.holes.push(hole);
        let region = vec![e];
        let mut c = Clipper::new(&region);
        assert!(c.contains(Pt::from_mm(1.0, 1.0)));
        assert!(!c.contains(Pt::from_mm(5.0, 5.0)));
        assert!(!c.contains(Pt::from_mm(11.0, 5.0)));
        let mut out = Pieces::default();
        c.clip(&[Pt::from_mm(-1.0, 5.0), Pt::from_mm(11.0, 5.0)], &mut out);
        assert_eq!(out.len(), 2);
        assert!((out.length_mm() - 8.0).abs() < 1e-3);
        assert!(out.ea.iter().chain(out.eb.iter()).all(|e| *e != FREE));
        // Starting inside: a free first end.
        let mut out = Pieces::default();
        c.clip(&[Pt::from_mm(1.0, 1.0), Pt::from_mm(2.0, 1.0), Pt::from_mm(2.0, 20.0)], &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out.ea[0], FREE);
        assert!((out.length_mm() - 10.0).abs() < 1e-3);
    }

    #[test]
    fn clip_through_vertex() {
        let region = vec![square(0.0, 0.0, 10.0, 10.0)];
        let mut c = Clipper::new(&region);
        let mut out = Pieces::default();
        // The diagonal passes through two corners.
        c.clip(&[Pt::from_mm(-1.0, -1.0), Pt::from_mm(11.0, 11.0)], &mut out);
        assert_eq!(out.len(), 1);
        assert!((out.length_mm() - 200f64.sqrt()).abs() < 1e-3);
    }

    #[test]
    fn link_joins_chains() {
        let p = |x: f64, y: f64| Pt::from_mm(x, y);
        let segs = vec![(p(0.0, 0.0), p(1.0, 0.0)), (p(2.0, 0.0), p(1.0, 0.0)), (p(5.0, 5.0), p(6.0, 5.0))];
        let l = link(&segs);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].len(), 3);
    }
}
