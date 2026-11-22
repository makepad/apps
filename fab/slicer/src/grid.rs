//! The shared spatial index: a fixed set of segments (each with a payload)
//! in a packed bucket grid, for exact queries without allocation.
//!
//! - Cells: a uniform grid sized from the segments' bounds and their mean
//!   length; each segment is listed in every cell its bounding box touches.
//!   Packed: one offsets array and one index array (no per-cell vectors).
//! - Rows: horizontal strips; each segment listed in every strip its y
//!   range touches, for exact winding numbers (only the strip of the query
//!   point is read).
//!
//! Every query answers exactly what a scan over all segments would:
//! [`SegGrid::nearest`] the same minimum distance and the same winner of a
//! tie, [`SegGrid::winding`] the same half-open winding sum as
//! [`crate::geom::winding_of_ring`] over the rings the segments came from,
//! [`SegGrid::visit_box`] every segment whose box meets the query box. The
//! index is immutable once built, so one can be shared by threads.

use crate::geom::*;

pub struct SegGrid<T: Copy> {
    segs: Vec<(Pt, Pt, T)>,
    min: Pt,
    cell: i64,
    nx: i64,
    ny: i64,
    /// cell → [cell_at[c], cell_at[c + 1]) in `cell_items`.
    cell_at: Vec<u32>,
    cell_items: Vec<u32>,
    row_h: i64,
    rows: usize,
    row_at: Vec<u32>,
    row_items: Vec<u32>,
}

fn pack(n: usize, mut each: impl FnMut(&mut dyn FnMut(usize, u32))) -> (Vec<u32>, Vec<u32>) {
    let mut count = vec![0u32; n + 1];
    each(&mut |c, _| count[c + 1] += 1);
    for i in 0..n {
        count[i + 1] += count[i];
    }
    let mut fill = count.clone();
    let mut items = vec![0u32; count[n] as usize];
    each(&mut |c, s| {
        items[fill[c] as usize] = s;
        fill[c] += 1;
    });
    (count, items)
}

impl<T: Copy> SegGrid<T> {
    pub fn new(segs: Vec<(Pt, Pt, T)>) -> Self {
        let mut b = BBox::empty();
        let mut total = 0.0;
        for &(a, c, _) in &segs {
            b.add(a);
            b.add(c);
            total += a.dist(c);
        }
        if segs.is_empty() {
            return Self { segs, min: Pt::default(), cell: 1, nx: 0, ny: 0, cell_at: vec![0], cell_items: Vec::new(), row_h: 1, rows: 0, row_at: vec![0], row_items: Vec::new() };
        }
        let (w, h) = ((b.max.x - b.min.x).max(1), (b.max.y - b.min.y).max(1));
        let n = segs.len() as f64;
        // Cells about two mean segment lengths, at most ~4 cells per segment.
        let mean = (total / n).max(1.0);
        let by_count = ((w as f64 * h as f64) / (4.0 * n)).sqrt();
        let cell = (2.0 * mean).max(by_count).max(1.0).ceil() as i64;
        let nx = w / cell + 1;
        let ny = h / cell + 1;
        let (cell_at, cell_items) = pack((nx * ny) as usize, |put| {
            for (i, &(a, c, _)) in segs.iter().enumerate() {
                let (x0, x1) = ((a.x.min(c.x) - b.min.x) / cell, (a.x.max(c.x) - b.min.x) / cell);
                let (y0, y1) = ((a.y.min(c.y) - b.min.y) / cell, (a.y.max(c.y) - b.min.y) / cell);
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        put((y * nx + x) as usize, i as u32);
                    }
                }
            }
        });
        let rows = ((n.sqrt().ceil()) as usize).clamp(1, 4096);
        let row_h = (h + rows as i64) / rows as i64;
        let (row_at, row_items) = pack(rows, |put| {
            for (i, &(a, c, _)) in segs.iter().enumerate() {
                if a.y == c.y {
                    continue;
                }
                let lo = ((a.y.min(c.y) - b.min.y) / row_h).clamp(0, rows as i64 - 1) as usize;
                let hi = ((a.y.max(c.y) - b.min.y) / row_h).clamp(0, rows as i64 - 1) as usize;
                for r in lo..=hi {
                    put(r, i as u32);
                }
            }
        });
        Self { segs, min: b.min, cell, nx, ny, cell_at, cell_items, row_h, rows, row_at, row_items }
    }

    /// Every ring edge of `region`, each with `tag` as its payload.
    pub fn of_regions(region: &[ExPolygon], tag: impl Fn(usize) -> T) -> Self {
        let mut segs = Vec::new();
        for (k, e) in region.iter().enumerate() {
            for r in e.rings() {
                let m = r.pts.len();
                for i in 0..m {
                    segs.push((r.pts[i], r.pts[(i + 1) % m], tag(k)));
                }
            }
        }
        Self::new(segs)
    }

    pub fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }

    pub fn segments(&self) -> &[(Pt, Pt, T)] {
        &self.segs
    }

    fn cell(&self, c: i64) -> &[u32] {
        &self.cell_items[self.cell_at[c as usize] as usize..self.cell_at[c as usize + 1] as usize]
    }

    /// The segment nearest `p` by [`dist_point_segment`], ties going to the
    /// smaller key of `key(payload)`: (distance, payload). `None` when empty.
    pub fn nearest<K: PartialOrd + Copy>(&self, p: Pt, key: impl Fn(T) -> K) -> Option<(f64, T)> {
        let mut best: Option<(f64, K, T)> = None;
        self.nearest_with(p, |a, b, t| {
            // A segment whose box is farther than the best so far (by more
            // than the rounded projection point can make up) can neither win
            // nor tie.
            if let Some(x) = best.as_ref() {
                let dx = (a.x.min(b.x) - p.x).max(p.x - a.x.max(b.x)).max(0) as f64;
                let dy = (a.y.min(b.y) - p.y).max(p.y - a.y.max(b.y)).max(0) as f64;
                let r = x.0 + 2.0;
                if dx * dx + dy * dy > r * r {
                    return x.0;
                }
            }
            let d = dist_point_segment(p, a, b);
            let k = key(t);
            if best.as_ref().is_none_or(|x| d < x.0 || (d == x.0 && k < x.1)) {
                best = Some((d, k, t));
            }
            best.as_ref().map_or(f64::INFINITY, |x| x.0)
        });
        best.map(|(d, _, t)| (d, t))
    }

    /// Visits candidate segments in rings of cells around `p`, nearest
    /// first; `f` returns the best distance found so far. Stops once no
    /// unvisited segment can be nearer than that (with a unit of slack for
    /// the rounded projection point), so every tie is seen.
    pub fn nearest_with(&self, p: Pt, mut f: impl FnMut(Pt, Pt, T) -> f64) {
        if self.segs.is_empty() {
            return;
        }
        let cx = ((p.x - self.min.x).div_euclid(self.cell)).clamp(0, self.nx - 1);
        let cy = ((p.y - self.min.y).div_euclid(self.cell)).clamp(0, self.ny - 1);
        let mut best = f64::INFINITY;
        let reach = self.nx.max(self.ny);
        for r in 0..=reach {
            let (y0, y1) = ((cy - r).max(0), (cy + r).min(self.ny - 1));
            let (x0, x1) = ((cx - r).max(0), (cx + r).min(self.nx - 1));
            for y in y0..=y1 {
                let edge_row = y == cy - r || y == cy + r;
                let mut x = x0;
                while x <= x1 {
                    for &i in self.cell(y * self.nx + x) {
                        let (a, b, t) = self.segs[i as usize];
                        best = f(a, b, t);
                    }
                    // Inside the ring only its two edge columns are new.
                    x = if edge_row || x != cx - r { x + 1 } else { (cx + r).max(x + 1) };
                }
            }
            // An unseen segment's entire box lies outside the visited block. Only sides
            // with unvisited cells beyond them bound its distance; grid borders do not.
            if x0 == 0 && x1 == self.nx - 1 && y0 == 0 && y1 == self.ny - 1 {
                break;
            }
            // These internal side lines lie within the segment bounds. abs_diff also
            // handles queries outside the grid without overflowing a signed distance.
            let mut delta = u64::MAX;
            if x0 > 0 { delta = delta.min(p.x.abs_diff(self.min.x + x0 * self.cell)); }
            if x1 < self.nx - 1 { delta = delta.min(p.x.abs_diff(self.min.x + (x1 + 1) * self.cell)); }
            if y0 > 0 { delta = delta.min(p.y.abs_diff(self.min.y + y0 * self.cell)); }
            if y1 < self.ny - 1 { delta = delta.min(p.y.abs_diff(self.min.y + (y1 + 1) * self.cell)); }
            if best + 2.0 < delta as f64 {
                break;
            }
        }
    }

    /// Every segment whose bounding box meets `bb` (each at most once per
    /// cell it is listed in: `f` may see a segment more than once).
    pub fn visit_box(&self, bb: BBox, mut f: impl FnMut(Pt, Pt, T)) {
        if self.segs.is_empty() {
            return;
        }
        let x0 = ((bb.min.x - self.min.x).div_euclid(self.cell)).clamp(0, self.nx - 1);
        let x1 = ((bb.max.x - self.min.x).div_euclid(self.cell)).clamp(0, self.nx - 1);
        let y0 = ((bb.min.y - self.min.y).div_euclid(self.cell)).clamp(0, self.ny - 1);
        let y1 = ((bb.max.y - self.min.y).div_euclid(self.cell)).clamp(0, self.ny - 1);
        if bb.max.x < self.min.x || bb.max.y < self.min.y || bb.min.x > self.min.x + self.nx * self.cell || bb.min.y > self.min.y + self.ny * self.cell {
            return;
        }
        for y in y0..=y1 {
            for x in x0..=x1 {
                for &i in self.cell(y * self.nx + x) {
                    let (a, b, t) = self.segs[i as usize];
                    let sb = BBox::of(&[a, b]);
                    if sb.overlaps(&bb) {
                        f(a, b, t);
                    }
                }
            }
        }
    }

    /// The winding number of `p` summed per payload group: `inside(group sum)`
    /// over consecutive segments of equal payload, true as soon as one group
    /// is inside. With the segments of `of_regions` (payload = region), this
    /// is `region.iter().any(|e| e.contains(p))`.
    pub fn contains_any(&self, p: Pt) -> bool
    where
        T: PartialEq,
    {
        if self.rows == 0 || p.y < self.min.y {
            return false;
        }
        let r = ((p.y - self.min.y) / self.row_h) as usize;
        if r >= self.rows {
            return false;
        }
        let items = &self.row_items[self.row_at[r] as usize..self.row_at[r + 1] as usize];
        let mut cur: Option<T> = None;
        let mut w = 0;
        for &i in items {
            let (a, b, t) = self.segs[i as usize];
            if cur != Some(t) {
                if w != 0 {
                    return true;
                }
                cur = Some(t);
                w = 0;
            }
            if a.y <= p.y {
                if b.y > p.y && orient(a, b, p) > 0 {
                    w += 1;
                }
            } else if b.y <= p.y && orient(a, b, p) < 0 {
                w -= 1;
            }
        }
        w != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pseudo(seed: &mut u64) -> i64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed % 20_000_000) as i64 - 10_000_000
    }

    /// Every query equals the scan over all segments.
    #[test]
    fn queries_equal_the_full_scan() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut region = Vec::new();
        for k in 0..20 {
            let (cx, cy) = (pseudo(&mut seed), pseudo(&mut seed));
            let r = 200_000 + (k * 37_000) as i64;
            let pts: Vec<Pt> = (0..(6 + k * 5)).map(|i| {
                let a = i as f64 / (6 + k * 5) as f64 * std::f64::consts::TAU;
                Pt::new(cx + (r as f64 * a.cos()) as i64, cy + (r as f64 * a.sin() * 0.6) as i64)
            }).collect();
            region.push(ExPolygon::new(Polygon::new(pts)));
        }
        let g = SegGrid::of_regions(&region, |k| k as u32);
        for _ in 0..4000 {
            let p = Pt::new(pseudo(&mut seed), pseudo(&mut seed));
            let scan = g.segments().iter().fold(None::<(f64, u32)>, |b, &(a, c, t)| {
                let d = dist_point_segment(p, a, c);
                if b.is_none_or(|x| d < x.0 || (d == x.0 && t < x.1)) { Some((d, t)) } else { b }
            });
            assert_eq!(g.nearest(p, |t| t), scan);
            assert_eq!(g.contains_any(p), region.iter().any(|e| e.contains(p)));
            let bb = BBox::of(&[p, Pt::new(p.x + 300_000, p.y + 150_000)]);
            let mut seen = Vec::new();
            g.visit_box(bb, |a, c, _| seen.push((a, c)));
            for &(a, c, _) in g.segments() {
                if BBox::of(&[a, c]).overlaps(&bb) {
                    assert!(seen.contains(&(a, c)));
                }
            }
        }
    }
}
