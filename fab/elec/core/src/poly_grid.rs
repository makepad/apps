//! Indexed containment, exact distance and bounded collision queries for a polygon set.
//! Segment collision queries visit cells within `L = min_gap + ceil(radius) + 1` nm of the
//! segment on each axis (with outward integer rounding), stopping at the first edge hit.

use crate::dist::{any_point, boundary_min, core_distance, inside_convex, rounded_gap};
use crate::geom::{BBox, Nm, P};
use crate::poly::{edges_winding, PolySet, Winding};
use crate::shape::{core_bbox, Core, Shape};

/// A polygon set with its edges bucketed on a uniform grid over its box. Every answer equals the
/// set's own: `contains` as `PolySet::contains`, `clearance` and `collides` as the free functions
/// with `Shape::Poly(set)` as the first shape, and `clearance_from` with it second, bit for bit.
/// Segment collision queries cost the cells and edge entries within
/// `L = min_gap + ceil(radius) + 1` nm of the segment on each axis, with outward integer rounding,
/// and stop at the first hit. Containment uses row edge lists. A fill's index is built once.
#[derive(Clone, Debug)]
pub struct PolyGrid {
    set: PolySet,
    bbox: BBox,
    edges: Vec<Edge>,
    outer_boxes: Vec<BBox>,
    cols: usize,
    rows: usize,
    w: i64,
    h: i64,
    cells: Buckets,
    row_edges: Buckets,
}

/// A collision found by a [`PolyGrid`] bounded query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// A core point lies in the set, or a set vertex lies in a filled query core.
    Inside,
    /// An edge of this grid is within the requested gap; reusable with [`PolyGrid::edge_hit`].
    Edge(u32),
}

impl PartialEq for PolyGrid {
    fn eq(&self, other: &Self) -> bool {
        self.set == other.set
    }
}

impl Eq for PolyGrid {}

#[derive(Clone, Debug)]
struct Edge {
    a: P,
    b: P,
    bbox: BBox,
    polygon: usize,
    ring: usize,
}

#[derive(Clone, Debug, Default)]
struct Buckets {
    offsets: Vec<usize>,
    edges: Vec<u32>,
}

impl Buckets {
    fn new(count: usize, entries: impl Iterator<Item = (usize, u32)> + Clone) -> Self {
        let mut offsets = vec![0; count + 1];
        for (bucket, _) in entries.clone() {
            offsets[bucket + 1] += 1;
        }
        for bucket in 0..count {
            offsets[bucket + 1] += offsets[bucket];
        }
        let mut edges = vec![0; offsets[count]];
        let mut next = offsets[..count].to_vec();
        // Entries arrive in edge order, so every bucket preserves ring and polygon order.
        for (bucket, edge) in entries {
            edges[next[bucket]] = edge;
            next[bucket] += 1;
        }
        Self { offsets, edges }
    }

    fn get(&self, bucket: usize) -> &[u32] {
        &self.edges[self.offsets[bucket]..self.offsets[bucket + 1]]
    }
}

#[derive(Clone, Copy)]
struct Block {
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
}

impl Block {
    fn cells(self, cols: usize) -> impl Iterator<Item = usize> + Clone {
        (self.y0..=self.y1).flat_map(move |y| (self.x0..=self.x1).map(move |x| y * cols + x))
    }
}

// Closed cell boxes share their boundary. The cell before an exact boundary belongs to the
// range too; subtract in i64, since both coordinates and a full box span can reach Nm's limits.
fn axis_cells(lo: Nm, hi: Nm, origin: Nm, step: i64, count: usize) -> (usize, usize) {
    let last = count as i64 - 1;
    let first = (lo as i64 - origin as i64 - 1).div_euclid(step).clamp(0, last);
    let end = (hi as i64 - origin as i64).div_euclid(step).clamp(0, last);
    (first as usize, end as usize)
}

impl PolyGrid {
    /// Owns `set` and builds compact, ordered cell and containment-row edge lists.
    pub fn new(set: PolySet) -> PolyGrid {
        let bbox = set.bbox();
        let mut edges = Vec::with_capacity(set.vertex_count());
        let mut outer_boxes = Vec::with_capacity(set.polygons().len());
        for (polygon, poly) in set.polygons().iter().enumerate() {
            outer_boxes.push(BBox::of_points(poly.outer.iter().copied()));
            for (ring, pts) in std::iter::once(&poly.outer).chain(poly.holes.iter()).enumerate() {
                for (i, &a) in pts.iter().enumerate() {
                    let b = pts[(i + 1) % pts.len()];
                    edges.push(Edge { a, b, bbox: BBox::new(a, b), polygon, ring });
                }
            }
        }
        let target = (edges.len() / 4).max(1);
        let width = bbox.width().max(1);
        let height = bbox.height().max(1);
        let cols = ((target as f64 * width as f64 / height as f64).sqrt().round() as usize)
            .clamp(1, target)
            .min(width as usize);
        let rows = ((target - 1) / cols + 1).min(height as usize);
        let mut grid = PolyGrid {
            set,
            bbox,
            edges,
            outer_boxes,
            cols,
            rows,
            w: (width / cols as i64).max(1),
            h: (height / rows as i64).max(1),
            cells: Buckets::default(),
            row_edges: Buckets::default(),
        };
        grid.cells = Buckets::new(
            cols * rows,
            grid.edges.iter().enumerate().flat_map(|(id, edge)| {
                grid.block(edge.bbox).cells(cols).map(move |cell| (cell, id as u32))
            }),
        );
        grid.row_edges = Buckets::new(
            rows,
            grid.edges.iter().enumerate().flat_map(|(id, edge)| {
                let (first, last) = axis_cells(edge.bbox.min.y, edge.bbox.max.y, bbox.min.y, grid.h, rows);
                (first..=last).map(move |row| (row, id as u32))
            }),
        );
        grid
    }

    /// The polygon set this index owns.
    pub fn set(&self) -> &PolySet {
        &self.set
    }

    /// The set's bounding box, including the empty box for an empty set.
    pub fn bbox(&self) -> BBox {
        self.bbox
    }

    /// True inside the filled set or on any boundary, exactly as `PolySet::contains`.
    pub fn contains(&self, p: P) -> bool {
        if self.set.is_empty() || !self.bbox.contains(p) {
            return false;
        }
        let row = ((p.y as i64 - self.bbox.min.y as i64) / self.h).min(self.rows as i64 - 1) as usize;
        let mut remaining = self.row_edges.get(row);
        while let Some(&id) = remaining.first() {
            let polygon = self.edges[id as usize].polygon;
            let end = remaining.partition_point(|&id| self.edges[id as usize].polygon == polygon);
            let (mut rings, tail) = remaining.split_at(end);
            remaining = tail;
            if !self.outer_boxes[polygon].contains(p) || self.edges[id as usize].ring != 0 {
                continue;
            }
            let end = rings.partition_point(|&id| self.edges[id as usize].ring == 0);
            match self.winding(&rings[..end], p) {
                Winding::On => return true,
                Winding::Out => continue,
                Winding::In => {}
            }
            rings = &rings[end..];
            let mut in_hole = false;
            while let Some(&id) = rings.first() {
                let ring = self.edges[id as usize].ring;
                let end = rings.partition_point(|&id| self.edges[id as usize].ring == ring);
                if self.winding(&rings[..end], p) == Winding::In {
                    in_hole = true;
                    break;
                }
                rings = &rings[end..];
            }
            if !in_hole {
                return true;
            }
        }
        false
    }

    /// Exact edge-to-edge gap in nm, rounded identically to `clearance(&Shape::Poly(set), other)`.
    pub fn clearance(&self, other: &Shape) -> Nm {
        let (core, r) = other.core();
        rounded_gap(self.distance(&core) - (0.0 + r))
    }

    /// Exact edge-to-edge gap in nm, rounded identically to `clearance(other, &Shape::Poly(set))`.
    /// For two polygon sets, preserves the first set's edge order and measures against this grid.
    pub fn clearance_from(&self, other: &Shape) -> Nm {
        let (core, r) = other.core();
        let distance = match &core {
            Core::Set(a) => self.set_distance_from(a),
            _ => self.distance(&core),
        };
        rounded_gap(distance - (r + 0.0))
    }

    /// True when the gap is strictly below `min_gap`, with the free `collides` function's guards.
    /// Segment queries visit cells within `L = min_gap + ceil(radius) + 1` nm on each axis
    /// (with outward integer rounding), and stop at the first hit instead of finding a minimum.
    pub fn collides(&self, other: &Shape, min_gap: Nm) -> bool {
        self.first_hit(other, min_gap).is_some()
    }

    /// A containment or edge witness, exactly when `collides(other, min_gap)` is true.
    /// Segment cores walk a bounded corridor from their first endpoint toward their second;
    /// other cores walk expanding bbox blocks. Both stop at the first edge under the gap.
    pub fn first_hit(&self, other: &Shape, min_gap: Nm) -> Option<Hit> {
        self.first_hit_impl(other, min_gap, false)
    }

    /// Like [`Self::first_hit`], but skips containment for a segment whose endpoints are known
    /// to be outside this set, including its boundary. The core must be a segment (or its
    /// degenerate point); this precondition is debug-asserted after the query guards.
    pub fn first_hit_clear_ends(&self, other: &Shape, min_gap: Nm) -> Option<Hit> {
        self.first_hit_impl(other, min_gap, true)
    }

    /// Test one edge witness from this grid, with the same guards and rounding as `collides`.
    /// A true result always implies `collides(other, min_gap)`; an invalid edge id returns false.
    pub fn edge_hit(&self, edge: u32, other: &Shape, min_gap: Nm) -> bool {
        if min_gap <= 0 || !self.bbox().inflate(min_gap).intersects(&other.bbox()) {
            return false;
        }
        let (core, r) = other.core();
        self.hit_edge(edge, &core, r, min_gap)
    }

    fn hit_edge(&self, id: u32, core: &Core<'_>, r: f64, min_gap: Nm) -> bool {
        self.edges.get(id as usize).is_some_and(|edge| {
            // Preserve measure's argument order and clearance's radius sum, including 0.0.
            rounded_gap(core_distance(&Core::Seg(edge.a, edge.b), core) - (0.0 + r)) < min_gap
        })
    }

    fn hit_cell(&self, cell: usize, core: &Core<'_>, r: f64, min_gap: Nm) -> Option<Hit> {
        self.cells.get(cell).iter().copied()
            .find(|&id| self.hit_edge(id, core, r, min_gap)).map(Hit::Edge)
    }

    fn first_hit_impl(&self, other: &Shape, min_gap: Nm, clear_ends: bool) -> Option<Hit> {
        if min_gap <= 0 || !self.bbox().inflate(min_gap).intersects(&other.bbox()) {
            return None;
        }
        if self.set.is_empty() {
            return None;
        }
        let (core, r) = other.core();
        if clear_ends {
            debug_assert!(match &core {
                Core::Seg(a, b) => !self.contains(*a) && !self.contains(*b),
                Core::Point(p) => !self.contains(*p),
                _ => false,
            }, "first_hit_clear_ends requires a segment with both endpoints outside the set");
        }
        // Shape permits negative widths/radii too. Even distance zero cannot hit in this case.
        if r < 0.0 && rounded_gap(0.0 - (0.0 + r)) >= min_gap {
            return None;
        }
        if !clear_ends && any_point(&core, |p| self.contains(p)) {
            return Some(Hit::Inside);
        }
        if let Core::Set(s) = &core {
            if s.is_empty() {
                return None;
            }
        }
        let limit = min_gap as i64 + r.ceil() as i64 + 1;
        if let Core::Seg(a, b) = &core {
            return self.segment_hit(*a, *b, &core, r, min_gap, limit);
        }
        let bbox = core_bbox(&core);
        if bbox.is_empty() {
            return None;
        }
        let mut block = self.block(bbox);
        if !clear_ends {
            match &core {
                Core::Convex(_) | Core::Set(_) => {
                    for cell in block.cells(self.cols) {
                        for &id in self.cells.get(cell) {
                            let a = self.edges[id as usize].a;
                            if bbox.contains(a) && match &core {
                                Core::Convex(pts) => inside_convex(a, pts),
                                Core::Set(s) => s.contains(a),
                                _ => unreachable!(),
                            } {
                                return Some(Hit::Inside);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // rounded_gap is monotone: testing each edge is equivalent to rounding the minimum.
        // A hit has distance < min_gap + r. The extra nm also covers floating-point slack.
        let hit_block = |block: Block| {
            block.cells(self.cols).find_map(|cell| self.hit_cell(cell, &core, r, min_gap))
        };
        if let Some(hit) = hit_block(block) {
            return Some(hit);
        }
        loop {
            if (block.x0 == 0 && block.x1 + 1 == self.cols && block.y0 == 0 && block.y1 + 1 == self.rows)
                || self.unseen_bound(block, bbox) > limit as f64
            {
                return None;
            }
            let next = Block {
                x0: block.x0.saturating_sub(1),
                x1: (block.x1 + 1).min(self.cols - 1),
                y0: block.y0.saturating_sub(1),
                y1: (block.y1 + 1).min(self.rows - 1),
            };
            // Same top, bottom and side strips as distance, with an early return per edge.
            for (visit, strip) in [
                (next.y0 < block.y0, Block { y1: next.y0, ..next }),
                (next.y1 > block.y1, Block { y0: next.y1, ..next }),
                (next.x0 < block.x0, Block { x0: next.x0, x1: next.x0, ..block }),
                (next.x1 > block.x1, Block { x0: next.x1, x1: next.x1, ..block }),
            ] {
                if visit {
                    if let Some(hit) = hit_block(strip) {
                        return Some(hit);
                    }
                }
            }
            block = next;
        }
    }

    fn segment_hit(&self, a: P, b: P, core: &Core<'_>, r: f64, min_gap: Nm, limit: i64) -> Option<Hit> {
        let nm = |v: i64| v.clamp(Nm::MIN as i64, Nm::MAX as i64) as Nm;
        let (p, q) = if a.y <= b.y { (a, b) } else { (b, a) };
        let (y0, y1) = axis_cells(
            nm(p.y as i64 - limit), nm(q.y as i64 + limit), self.bbox.min.y, self.h, self.rows,
        );
        for row in 0..=y1 - y0 {
            let y = if a.y <= b.y { y0 + row } else { y1 - row };
            let bottom = self.bbox.min.y as i64 + y as i64 * self.h;
            // The last cell absorbs the division remainder, so it may be taller than h.
            let top = if y + 1 == self.rows { self.bbox.max.y as i64 } else { bottom + self.h };
            let lo = (p.y as i64).max(bottom - limit);
            let hi = (q.y as i64).min(top + limit);
            if lo > hi {
                continue;
            }
            let (left, right) = if p.y == q.y {
                (p.x.min(q.x) as i64, p.x.max(q.x) as i64)
            } else {
                let dy = q.y as i128 - p.y as i128;
                let dx = q.x as i128 - p.x as i128;
                let x = |y: i64| p.x as i128 * dy + (y as i128 - p.y as i128) * dx;
                let (x0, x1) = (x(lo), x(hi));
                let (left, right) = (x0.min(x1), x0.max(x1));
                (
                    left.div_euclid(dy) as i64,
                    (right.div_euclid(dy) + i128::from(right.rem_euclid(dy) != 0)) as i64,
                )
            };
            // Exact rational clipping rounds the x extent outward; grow it by L + 1.
            let (x0, x1) = axis_cells(
                nm(left - limit - 1), nm(right + limit + 1), self.bbox.min.x, self.w, self.cols,
            );
            for col in 0..=x1 - x0 {
                let x = if a.x <= b.x { x0 + col } else { x1 - col };
                if let Some(hit) = self.hit_cell(y * self.cols + x, core, r, min_gap) {
                    return Some(hit);
                }
            }
        }
        None
    }

    fn winding(&self, edges: &[u32], p: P) -> Winding {
        edges_winding(
            edges.iter().map(|&id| {
                let edge = &self.edges[id as usize];
                (edge.a, edge.b)
            }),
            p,
        )
    }

    fn block(&self, bbox: BBox) -> Block {
        let (x0, x1) = axis_cells(bbox.min.x, bbox.max.x, self.bbox.min.x, self.w, self.cols);
        let (y0, y1) = axis_cells(bbox.min.y, bbox.max.y, self.bbox.min.y, self.h, self.rows);
        Block { x0, x1, y0, y1 }
    }

    fn set_distance_from(&self, a: &PolySet) -> f64 {
        // Mirror set_distance(a, &Core::Set(self.set())): its first containment test visits self.
        if a.is_empty() {
            return f64::INFINITY;
        }
        let bbox = a.bbox();
        for cell in self.block(bbox).cells(self.cols) {
            for &id in self.cells.get(cell) {
                let p = self.edges[id as usize].a;
                if bbox.contains(p) && a.contains(p) {
                    return 0.0;
                }
            }
        }
        if self.set.is_empty() {
            return f64::INFINITY;
        }
        if a.rings().flatten().any(|&p| self.contains(p)) {
            return 0.0;
        }
        // Same ring and edge order as dist::edges, and self.bbox is core_bbox(&Core::Set(self.set())).
        let boundary = a.rings().flat_map(|pts| {
            pts.iter().copied().zip(pts.iter().copied().cycle().skip(1))
        });
        boundary_min(boundary, self.bbox, |p, q| self.distance(&Core::Seg(p, q)))
    }

    fn distance(&self, core: &Core<'_>) -> f64 {
        if self.set.is_empty() {
            return f64::INFINITY;
        }
        if any_point(core, |p| self.contains(p)) {
            return 0.0;
        }
        if let Core::Set(s) = core {
            if s.is_empty() {
                return f64::INFINITY;
            }
        }
        let bbox = core_bbox(core);
        if bbox.is_empty() {
            return f64::INFINITY;
        }
        let mut block = self.block(bbox);
        if let Core::Convex(pts) = core {
            for cell in block.cells(self.cols) {
                for &id in self.cells.get(cell) {
                    let a = self.edges[id as usize].a;
                    if bbox.contains(a) && inside_convex(a, pts) {
                        return 0.0;
                    }
                }
            }
        }
        if let Core::Set(s) = core {
            for cell in block.cells(self.cols) {
                for &id in self.cells.get(cell) {
                    let a = self.edges[id as usize].a;
                    if bbox.contains(a) && s.contains(a) {
                        return 0.0;
                    }
                }
            }
        }
        let mut best = f64::INFINITY;
        self.measure(block, core, bbox, &mut best);
        loop {
            if best == 0.0
                || (block.x0 == 0 && block.x1 + 1 == self.cols && block.y0 == 0 && block.y1 + 1 == self.rows)
                || self.unseen_bound(block, bbox) > best + best.abs() * 1e-12 + 1e-9
            {
                return best;
            }
            let next = Block {
                x0: block.x0.saturating_sub(1),
                x1: (block.x1 + 1).min(self.cols - 1),
                y0: block.y0.saturating_sub(1),
                y1: (block.y1 + 1).min(self.rows - 1),
            };
            // Top and bottom include the corners; the side strips use only the old rows.
            if next.y0 < block.y0 {
                self.measure(Block { y1: next.y0, ..next }, core, bbox, &mut best);
            }
            if next.y1 > block.y1 {
                self.measure(Block { y0: next.y1, ..next }, core, bbox, &mut best);
            }
            if next.x0 < block.x0 {
                self.measure(Block { x0: next.x0, x1: next.x0, ..block }, core, bbox, &mut best);
            }
            if next.x1 > block.x1 {
                self.measure(Block { x0: next.x1, x1: next.x1, ..block }, core, bbox, &mut best);
            }
            block = next;
        }
    }

    fn measure(&self, block: Block, core: &Core<'_>, bbox: BBox, best: &mut f64) {
        if *best == 0.0 {
            return;
        }
        for cell in block.cells(self.cols) {
            for &id in self.cells.get(cell) {
                let edge = &self.edges[id as usize];
                // Use boundary_distance's exact box separation and its outward floating slack.
                let nearest = P::new(
                    edge.bbox.min.x.clamp(bbox.min.x, bbox.max.x),
                    edge.bbox.min.y.clamp(bbox.min.y, bbox.max.y),
                );
                let limit = *best + (*best).abs() * 1e-12 + 1e-9;
                if edge.bbox.dist2_to(nearest) as f64 > limit * limit {
                    continue;
                }
                *best = (*best).min(core_distance(&Core::Seg(edge.a, edge.b), core));
                if *best == 0.0 {
                    return;
                }
            }
        }
    }

    fn unseen_bound(&self, block: Block, bbox: BBox) -> f64 {
        // An unseen edge's closed box misses the entire visited rectangle. Its distance is
        // bounded by one of these four half-planes; a side at the grid edge has no unseen edges.
        let mut bound = f64::INFINITY;
        if block.x0 > 0 {
            let side = self.bbox.min.x as i64 + block.x0 as i64 * self.w;
            bound = bound.min((bbox.min.x as i64 - side).max(0) as f64);
        }
        if block.x1 + 1 < self.cols {
            let side = self.bbox.min.x as i64 + (block.x1 + 1) as i64 * self.w;
            bound = bound.min((side - bbox.max.x as i64).max(0) as f64);
        }
        if block.y0 > 0 {
            let side = self.bbox.min.y as i64 + block.y0 as i64 * self.h;
            bound = bound.min((bbox.min.y as i64 - side).max(0) as f64);
        }
        if block.y1 + 1 < self.rows {
            let side = self.bbox.min.y as i64 + (block.y1 + 1) as i64 * self.h;
            bound = bound.min((side - bbox.max.y as i64).max(0) as f64);
        }
        bound
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{clearance, collides, mm, Angle};

    fn assert_hits(grid: &PolyGrid, shape: &Shape, gap: Nm, min_gap: Nm, case: &str) -> bool {
        let expected = min_gap > 0 && grid.bbox().inflate(min_gap).intersects(&shape.bbox())
            && gap < min_gap;
        let hit = grid.first_hit(shape, min_gap);
        assert_eq!(hit.is_some(), expected, "{case}: first_hit matches guarded clearance at {min_gap}");
        if let Some(Hit::Edge(id)) = hit {
            assert!(grid.edge_hit(id, shape, min_gap), "{case}: first_hit edge is a witness at {min_gap}");
        }
        let collision = grid.collides(shape, min_gap);
        assert_eq!(collision, expected, "{case}: collides matches guarded clearance at {min_gap}");
        for id in 0..grid.edges.len().min(32) as u32 {
            assert!(
                !grid.edge_hit(id, shape, min_gap) || collision,
                "{case}: edge_hit {id} implies collides at {min_gap}",
            );
        }
        let (core, _) = shape.core();
        let clear_ends = match core {
            Core::Seg(a, b) => !grid.contains(a) && !grid.contains(b),
            Core::Point(p) => !grid.contains(p),
            _ => false,
        };
        if clear_ends {
            let hit = grid.first_hit_clear_ends(shape, min_gap);
            assert_eq!(hit.is_some(), expected, "{case}: clear ends matches guarded clearance at {min_gap}");
            if let Some(Hit::Edge(id)) = hit {
                assert!(grid.edge_hit(id, shape, min_gap), "{case}: clear ends edge is a witness at {min_gap}");
            }
        }
        collision
    }

    #[test]
    fn indexed_queries_match_polygon_set() {
        let rect = |x0, y0, x1, y1| PolySet::rect(BBox::new(P::mm(x0, y0), P::mm(x1, y1)));
        let circle = |x, y, radius, error| {
            Shape::Circle { c: P::mm(x, y), r: mm(radius) }.to_polyset(error)
        };
        let outer = rect(-10.0, -10.0, 10.0, 10.0);
        let hole = outer.difference(&rect(-4.0, -4.0, 4.0, 4.0));
        let islands = hole.union(&rect(-1.0, -1.0, 1.0, 1.0)).union(&rect(13.0, -2.0, 16.0, 2.0));
        let curved = rect(-6.0, -4.0, 4.0, 6.0)
            .union(&circle(4.0, 1.0, 4.0, 20_000))
            .difference(&circle(0.0, 0.0, 1.0, 10_000));
        let mut pads = Vec::new();
        for y in 0..5 {
            for x in 0..6 {
                pads.push(circle(-7.5 + 3.0 * x as f64, -6.0 + 3.0 * y as f64, 0.55, 500));
            }
        }
        let fill = outer.difference(&PolySet::union_all(&pads));
        assert!(fill.vertex_count() >= 2_000, "pad field: several thousand vertices");
        let mut square_holes = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                let x = -7.0 + 4.0 * x as f64;
                let y = -6.0 + 4.0 * y as f64;
                square_holes.push(rect(x, y, x + 2.0, y + 2.0));
            }
        }
        let aligned = outer.difference(&PolySet::union_all(&square_holes));
        let sets = [
            ("empty", PolySet::new()),
            ("rectangle", outer),
            ("rectangular hole", hole),
            ("island in hole and disjoint polygon", islands),
            ("circle union and difference", curved),
            ("pad field", fill),
            ("hole edges on shared cell boundaries", aligned),
        ];
        let templates = [
            ("point", Shape::Circle { c: P::ZERO, r: 0 }),
            ("circle", Shape::Circle { c: P::ZERO, r: mm(0.25) }),
            (
                "segment",
                Shape::Segment { a: P::mm(-0.5, 0.0), b: P::mm(0.5, 0.0), w: 3 },
            ),
            (
                "zero-width segment",
                Shape::Segment { a: P::mm(-0.5, 0.0), b: P::mm(0.5, 0.0), w: 0 },
            ),
            (
                "segment with radius",
                Shape::Segment { a: P::mm(-0.5, 0.0), b: P::mm(0.5, 0.0), w: mm(0.5) },
            ),
            ("zero-length segment", Shape::Segment { a: P::ZERO, b: P::ZERO, w: 3 }),
            ("zero-length zero-width segment", Shape::Segment { a: P::ZERO, b: P::ZERO, w: 0 }),
            (
                "arc",
                Shape::Arc { a: P::mm(0.5, 0.0), mid: P::mm(0.0, 0.5), b: P::mm(-0.5, 0.0), w: 3 },
            ),
            (
                "rotated rectangle",
                Shape::Rect { c: P::ZERO, half: P::mm(0.6, 0.3), rot: Angle::deg(31.0), corner: 0 },
            ),
            (
                "rotated rounded rectangle",
                Shape::Rect {
                    c: P::ZERO,
                    half: P::mm(0.6, 0.3),
                    rot: Angle::deg(31.0),
                    corner: mm(0.1),
                },
            ),
            ("oval", Shape::Oval { c: P::ZERO, half: P::mm(0.6, 0.3), rot: Angle::deg(67.0) }),
            ("polygon", Shape::Poly(rect(-0.3, -0.2, 0.3, 0.2))),
        ];
        let placements = [
            ("centre or hole", P::ZERO),
            ("copper interior", P::mm(8.0, 8.0)),
            ("outer edge", P::mm(10.0, 0.0)),
            ("hole edge", P::mm(4.0, 0.0)),
            ("pad hole", P::mm(1.5, 0.0)),
            ("far outside", P::mm(100.0, 100.0)),
        ];
        let mut queries = Vec::new();
        for (kind, template) in &templates {
            for (place, offset) in placements {
                queries.push((format!("{kind} at {place}"), template.translated(offset)));
            }
        }
        // Exercise both row/column directions, horizontal bands and vertical clipping.
        for (a, b) in [
            (P::mm(-12.0, -11.0), P::mm(12.0, 11.0)),
            (P::mm(-12.0, 11.0), P::mm(12.0, -11.0)),
            (P::mm(-12.0, 10.25), P::mm(12.0, 10.25)),
            (P::mm(10.25, -12.0), P::mm(10.25, 12.0)),
            (P::new(Nm::MIN, Nm::MIN), P::new(Nm::MAX, Nm::MAX)),
        ] {
            for (a, b) in [(a, b), (b, a)] {
                for w in [0, mm(0.5)] {
                    queries.push((format!("corridor {a:?} to {b:?} width {w}"), Shape::Segment { a, b, w }));
                }
            }
        }
        queries.extend([
            ("empty polygon".to_owned(), Shape::Poly(PolySet::new())),
            ("polygon with hole".to_owned(), Shape::Poly(sets[2].1.clone())),
            (
                "exact circle contact".to_owned(),
                Shape::Circle { c: P::mm(10.25, 0.0), r: mm(0.25) },
            ),
            (
                "exact segment contact".to_owned(),
                Shape::Segment { a: P::mm(10.0, 0.0), b: P::mm(12.0, 0.0), w: 0 },
            ),
            (
                "arc contact at extremum".to_owned(),
                Shape::Arc { a: P::mm(12.0, -2.0), mid: P::mm(10.0, 0.0), b: P::mm(12.0, 2.0), w: 0 },
            ),
            (
                "arc crossing with defining points outside".to_owned(),
                Shape::Arc { a: P::mm(12.0, 0.0), mid: P::mm(0.0, 12.0), b: P::mm(-12.0, 0.0), w: 0 },
            ),
            (
                "convex encloses entire set".to_owned(),
                Shape::Rect { c: P::ZERO, half: P::mm(30.0, 30.0), rot: Angle::deg(13.0), corner: 0 },
            ),
            (
                "convex crosses with vertices outside".to_owned(),
                Shape::Rect { c: P::ZERO, half: P::mm(12.0, 0.1), rot: Angle::ZERO, corner: 0 },
            ),
            (
                "rounded rectangle reduced to segment".to_owned(),
                Shape::Rect {
                    c: P::mm(-12.0, 0.0),
                    half: P::mm(0.6, 0.3),
                    rot: Angle::ZERO,
                    corner: mm(0.3),
                },
            ),
            ("maximum coordinate".to_owned(), Shape::Circle { c: P::new(Nm::MAX, Nm::MAX), r: 0 }),
            ("minimum coordinate".to_owned(), Shape::Circle { c: P::new(Nm::MIN, Nm::MIN), r: 0 }),
            (
                "negative-width contained core".to_owned(),
                Shape::Segment { a: P::mm(7.0, 7.0), b: P::mm(9.0, 9.0), w: -mm(0.5) },
            ),
            (
                "negative-width crossing core".to_owned(),
                Shape::Segment { a: P::mm(-12.0, -12.0), b: P::mm(12.0, 12.0), w: -3 },
            ),
        ]);
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        for (name, set) in sets {
            let grid = PolyGrid::new(set.clone());
            let reference = Shape::Poly(set.clone());
            assert_eq!(grid.set(), &set, "{name}: owned set");
            assert_eq!(grid.bbox(), set.bbox(), "{name}: bounding box");
            assert_eq!(grid, grid.clone(), "{name}: clone equality");
            assert_eq!(grid, PolyGrid::new(set.clone()), "{name}: equality depends on the set");
            assert_eq!(grid == PolyGrid::new(PolySet::new()), set.is_empty(), "{name}: unequal sets");

            let mut points = vec![
                P::ZERO,
                P::mm(8.0, 8.0),
                P::mm(4.0, 0.0),
                P::mm(-4.0, 0.0),
                P::mm(10.0, 0.0),
                P::mm(10.0, 10.0),
                P::mm(1.5, 0.0),
                P::mm(14.0, 0.0),
                P::mm(100.0, -100.0),
                P::new(Nm::MIN, Nm::MIN),
                P::new(Nm::MAX, Nm::MAX),
            ];
            for ring in set.rings() {
                for i in [0, ring.len() / 2] {
                    let a = ring[i];
                    let b = ring[(i + 1) % ring.len()];
                    points.push(a);
                    points.push(BBox::new(a, b).center());
                    points.push(P::new(a.x, a.y - 1));
                    points.push(P::new(a.x, a.y + 1));
                }
            }
            if !set.is_empty() {
                for col in [0, grid.cols / 2, grid.cols - 1] {
                    for row in [0, grid.rows / 2, grid.rows - 1] {
                        let x = (grid.bbox.min.x as i64 + col as i64 * grid.w) as Nm;
                        let y = (grid.bbox.min.y as i64 + row as i64 * grid.h) as Nm;
                        for delta in [-1, 0, 1] {
                            points.push(P::new(x, y + delta));
                            points.push(P::new(x + delta, y));
                        }
                    }
                }
            }
            for _ in 0..16 {
                let mut coordinate = || {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    (seed % 40_000_001) as Nm - 20_000_000
                };
                points.push(P::new(coordinate(), coordinate()));
            }
            for (case, p) in points.into_iter().enumerate() {
                assert_eq!(grid.contains(p), set.contains(p), "{name}: contains point {case} {p:?}");
            }
            for (case, shape) in &queries {
                let (core, _) = shape.core();
                assert_eq!(
                    grid.distance(&core).to_bits(),
                    core_distance(&Core::Set(&set), &core).to_bits(),
                    "{name}, {case}: core distance bits",
                );
                let gap = clearance(&reference, shape);
                assert_eq!(grid.clearance(shape), gap, "{name}, {case}: clearance");
                assert_eq!(
                    grid.clearance_from(shape), clearance(shape, &reference), "{name}, {case}: clearance from",
                );
                for g in [-1, 0, 1, 3, mm(0.1), mm(1.0), mm(100.0), gap, gap.saturating_add(1)] {
                    assert_eq!(
                        assert_hits(&grid, shape, gap, g, &format!("{name}, {case}")),
                        collides(&reference, shape, g),
                        "{name}, {case}: collides at gap {g}",
                    );
                }
            }
        }
    }

    #[test]
    fn random_clearances_match_both_argument_orders() {
        fn random(seed: &mut u64, lo: Nm, hi: Nm) -> Nm {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 7;
            *seed ^= *seed << 17;
            lo + (*seed % (hi - lo + 1) as u64) as Nm
        }

        fn polygon(seed: &mut u64, c: P, unit: Nm) -> PolySet {
            let p = |x, y| P::new(c.x + x * unit, c.y + y * unit);
            // Random bevels and a diamond hole give oblique edges while leaving known nesting regions.
            let outer = vec![
                p(-100, -random(seed, 60, 90)), p(-random(seed, 60, 90), -100),
                p(random(seed, 60, 90), -100), p(100, -random(seed, 60, 90)),
                p(100, random(seed, 60, 90)), p(random(seed, 60, 90), 100),
                p(-random(seed, 60, 90), 100), p(-100, random(seed, 60, 90)),
            ];
            let hole = vec![
                p(0, -random(seed, 25, 40)), p(random(seed, 25, 40), 0),
                p(0, random(seed, 25, 40)), p(-random(seed, 25, 40), 0),
            ];
            PolySet::from_rings_even_odd(&[outer, hole])
        }

        let mut seed = 0x6a09_e667_f3bc_c909u64;
        let (mut nested_in, mut nested_out, mut hole_in, mut hole_out) = (0, 0, 0, 0);
        // Small cases with bounded vertex counts keep this brute-force comparison inexpensive.
        for case in 0..20 {
            let c = P::new(random(&mut seed, -500_000_000, 500_000_000), random(&mut seed, -500_000_000, 500_000_000));
            let unit = random(&mut seed, 100, 100_000);
            let p = |x, y| P::new(c.x + x * unit, c.y + y * unit);
            let rect = |x0, y0, x1, y1| PolySet::rect(BBox::new(p(x0, y0), p(x1, y1)));
            let set = polygon(&mut seed, c, unit);
            let grid = PolyGrid::new(set.clone());
            let reference = Shape::Poly(set.clone());
            assert_eq!(set.polygons()[0].holes.len(), 1, "set {case}: generated hole");
            let hole = PolySet::from_ring_nonzero(set.polygons()[0].holes[0].clone());

            let x = random(&mut seed, -80, -65);
            let y = random(&mut seed, -10, 10);
            let small = rect(x, y, x + random(&mut seed, 5, 10), y + random(&mut seed, 5, 10));
            let in_hole = rect(-5, -4, random(&mut seed, 3, 8), random(&mut seed, 3, 8));
            let extent = random(&mut seed, 140, 180);
            let big = rect(-extent, -extent, extent, extent);
            let void = rect(-130, -130, 130, 130);
            let around = rect(-220, -220, 220, 220).difference(&void);
            if small.rings().flatten().all(|&p| set.contains(p))
                && !set.rings().flatten().any(|&p| small.contains(p))
            {
                nested_in += 1;
            }
            if set.rings().flatten().all(|&p| big.contains(p))
                && !big.rings().flatten().any(|&p| set.contains(p))
            {
                nested_out += 1;
            }
            if in_hole.rings().flatten().all(|&p| hole.contains(p)) {
                hole_in += 1;
            }
            if set.rings().flatten().all(|&p| void.contains(p)) {
                hole_out += 1;
            }

            let arc_c = p(random(&mut seed, -180, 180), random(&mut seed, -180, 180));
            let radius = random(&mut seed, 1, 40 * unit);
            let other_c = p(random(&mut seed, -250, 250), random(&mut seed, -250, 250));
            let queries = [
                (
                    "circle",
                    Shape::Circle {
                        c: p(random(&mut seed, -180, 180), random(&mut seed, -180, 180)),
                        r: random(&mut seed, 0, 25 * unit),
                    },
                ),
                (
                    "segment with odd width",
                    Shape::Segment {
                        a: p(random(&mut seed, -180, 180), random(&mut seed, -180, 180)),
                        b: p(random(&mut seed, -180, 180), random(&mut seed, -180, 180)),
                        w: 2 * random(&mut seed, 0, unit) + 1,
                    },
                ),
                (
                    "zero-width segment",
                    Shape::Segment { a: arc_c, b: other_c, w: 0 },
                ),
                (
                    "zero-length segment with radius",
                    Shape::Segment { a: arc_c, b: arc_c, w: 2 * radius + 1 },
                ),
                (
                    "arc with odd width",
                    Shape::Arc {
                        a: arc_c.add(P::new(radius, 0)), mid: arc_c.add(P::new(0, radius)),
                        b: arc_c.add(P::new(-radius, 0)), w: 2 * random(&mut seed, 0, unit) + 1,
                    },
                ),
                (
                    "rotated rounded rectangle",
                    Shape::Rect {
                        c: p(random(&mut seed, -180, 180), random(&mut seed, -180, 180)),
                        half: P::new(random(&mut seed, 5, 30) * unit, random(&mut seed, 5, 30) * unit),
                        rot: Angle::deg(random(&mut seed, 1, 359) as f64), corner: random(&mut seed, 0, unit),
                    },
                ),
                ("inside polygon", Shape::Poly(small)),
                ("inside hole", Shape::Poly(in_hole)),
                ("grid inside polygon", Shape::Poly(big)),
                ("grid inside hole", Shape::Poly(around)),
                ("overlapping", Shape::Poly(rect(90, -10, random(&mut seed, 110, 130), 10))),
                ("touching", Shape::Poly(rect(100, -10, random(&mut seed, 110, 130), 10))),
                ("disjoint", Shape::Poly(rect(100 + random(&mut seed, 1, 10), -10, 140, 10))),
                ("random polygon with hole", Shape::Poly(polygon(&mut seed, other_c, unit))),
                ("crossing with vertices outside", Shape::Poly(rect(-120, 45, 120, 55))),
                ("empty polygon", Shape::Poly(PolySet::new())),
            ];
            for (name, other) in queries {
                let forward = clearance(&reference, &other);
                let reverse = clearance(&other, &reference);
                assert_eq!(grid.clearance(&other), forward, "set {case}, {name}: grid-first clearance");
                assert_eq!(grid.clearance_from(&other), reverse, "set {case}, {name}: grid-second clearance");
                let (core, _) = other.core();
                assert_eq!(
                    grid.distance(&core).to_bits(), core_distance(&Core::Set(&set), &core).to_bits(),
                    "set {case}, {name}: grid-first core distance bits",
                );
                for g in [-1, 0, 1, 3, mm(0.1), mm(1.0), mm(100.0), forward, forward.saturating_add(1)] {
                    assert_hits(&grid, &other, forward, g, &format!("set {case}, {name}"));
                }
                if let Core::Set(a) = core {
                    assert_eq!(
                        grid.set_distance_from(a).to_bits(), core_distance(&Core::Set(a), &Core::Set(&set)).to_bits(),
                        "set {case}, {name}: grid-second core distance bits",
                    );
                }
                match name {
                    "inside polygon" | "grid inside polygon" | "overlapping" | "touching"
                    | "crossing with vertices outside" => {
                        assert_eq!((forward, reverse), (0, 0), "set {case}, {name}: contact in both orders");
                    }
                    "inside hole" | "grid inside hole" | "disjoint" => {
                        assert!(forward > 0 && reverse > 0, "set {case}, {name}: positive gaps in both orders");
                    }
                    _ => {}
                }
            }
        }
        assert!(nested_in > 0, "nested polygon coverage: other inside grid with no grid vertex inside other");
        assert!(nested_out > 0, "nested polygon coverage: grid inside other with no other vertex inside grid");
        assert!(hole_in > 0, "nested hole coverage: other inside grid's hole");
        assert!(hole_out > 0, "nested hole coverage: grid inside other's hole");
    }
}
