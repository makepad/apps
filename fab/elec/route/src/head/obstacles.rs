//! The band's obstacle callbacks over a base and this answer's overlay.

use crate::embed::band::{GapInfo, Obstacles};
use crate::embed::hull::{self, Hull};
use crate::world::{query_key, RId, RItem, WRef, World};
use fab_elec_board::{allowed_need, check_to, required_to, ConnItem, Copper, RuleMatrix};
use fab_elec_core::shape::Core;
use fab_elec_core::{clearance, BBox, ClassId, ItemId, LayerId, NetId, Nm, Shape, P};
use std::collections::{BTreeMap, HashMap};
use std::ops::{ControlFlow, Range};
use std::sync::Arc;
use std::time::Instant;

/// Hulls kept between embeds. Base pieces' are kept across samples of one generation, by piece; a
/// base piece the overlay moved (`World::add_moved`) has its base piece's hulls, moved. Pieces an
/// overlay added (laid tracks) are kept by what they are (shape, required gap, width), which no
/// change to a piece can leave stale: a changed piece is another key. Added and translated hulls
/// are dropped with each newer sample (`clear_added`), so they live for one answer and its wave.
#[derive(Default)]
pub struct HullCache {
    /// Base hulls shared across entries, by piece, layer, head width and required gap.
    parts: BTreeMap<(RId, LayerId, Nm, Nm), Arc<[Hull]>>,
    /// Added hulls by (required gap, head width), then by shape so a hit can borrow the shape.
    added: HashMap<(Nm, Nm), HashMap<Shape, Arc<[Hull]>>>,
    /// Translated hulls by base piece, offset, required gap and head width for this sample.
    moved: HashMap<(RId, P, Nm, Nm), Arc<[Hull]>>,
    /// Grid scratch for the current run's gathered set. It holds no answers across runs.
    grid: PieceGrid,
    built: usize,
    built_ns: u64,
    built_added: usize,
}

impl HullCache {
    pub fn clear(&mut self) {
        self.parts.clear();
        self.clear_added();
    }

    /// Drop the hulls of added and moved pieces (a newer sample's answer starts).
    pub fn clear_added(&mut self) {
        self.added.clear();
        self.moved.clear();
    }

    /// How many obstacles' hulls were built, not found here (a count for benchmarks).
    pub fn built(&self) -> usize {
        self.built
    }

    /// The time spent building them, in nanoseconds.
    pub fn built_ns(&self) -> u64 {
        self.built_ns
    }

    /// How many of them were of pieces an overlay added, not base pieces moved.
    pub fn built_added(&self) -> usize {
        self.built_added
    }

    fn hulls(
        &mut self, world: &World, entry: &Entry<'_>, layer: LayerId, w: Nm,
    ) -> Option<Arc<[Hull]>> {
        let shape = entry.item.shape();
        let need = entry.need;
        let moved = world.moved_from(entry.reference);
        let base = match entry.reference {
            WRef::Base(id) => Some(id),
            WRef::Over(_) => moved.map(|(id, _)| id),
        };
        let key = base.map(|id| (id, layer, w, need));
        let kept = match key {
            Some(key) => self.parts.get(&key),
            None => self.added.get(&(need, w)).and_then(|shapes| shapes.get(shape)),
        };
        let hulls = if let Some(hulls) = kept { hulls } else {
            let t = Instant::now();
            let original = moved.and_then(|(id, _)| world.base.get(id)).map(RItem::shape);
            let built = checked_parts(original.unwrap_or(shape), need, w);
            self.built_ns += t.elapsed().as_nanos() as u64;
            let hulls: Arc<[Hull]> = built?.into();
            self.built += 1;
            match key {
                Some(key) => self.parts.entry(key).or_insert(hulls),
                None => {
                    self.built_added += 1;
                    self.added.entry((need, w)).or_default().entry(shape.clone()).or_insert(hulls)
                }
            }
        };
        // A moved piece still consults this layer's base cache before reusing the translation.
        let hulls = match moved {
            Some((id, by)) => self.moved.entry((id, by, need, w)).or_insert_with(|| {
                hulls.iter().map(|hull| hull.translated(by)).collect()
            }),
            None => hulls,
        };
        Some(Arc::clone(hulls))
    }
}

struct Entry<'a> {
    reference: WRef,
    /// The piece returned for `reference` by the immutable world's query.
    item: &'a RItem,
    need: Nm,
    /// Its shared hulls in part order, once `near_items` has found them.
    hulls: Option<Arc<[Hull]>>,
}

/// The world's pieces in `area` in query order, each with its entry once it has one. An outside
/// call extends the area by merging the pieces from its new strips into that order.
struct Gathered<'a> {
    area: BBox,
    pieces: Vec<(WRef, &'a RItem, Option<u32>)>,
    /// Calls answered since the initial gather, including calls that extended the area.
    calls: usize,
}

/// Disjoint closed integer strips covering the part of `outer` outside its contained box `inner`.
fn strips(outer: BBox, inner: BBox) -> [BBox; 4] {
    let mut strips = [BBox::EMPTY; 4];
    if inner.is_empty() {
        strips[0] = outer;
        return strips;
    }
    if outer.min.y < inner.min.y {
        strips[0] = BBox::new(outer.min, P::new(outer.max.x, inner.min.y - 1));
    }
    if outer.max.y > inner.max.y {
        strips[1] = BBox::new(P::new(outer.min.x, inner.max.y + 1), outer.max);
    }
    if outer.min.x < inner.min.x {
        strips[2] = BBox::new(P::new(outer.min.x, inner.min.y), P::new(inner.min.x - 1, inner.max.y));
    }
    if outer.max.x > inner.max.x {
        strips[3] = BBox::new(P::new(inner.max.x + 1, inner.min.y), P::new(outer.max.x, inner.max.y));
    }
    strips
}

/// Smaller gathered sets use the linear scan because the grid costs more than it saves there.
const INDEX_FROM: usize = 64;

/// Each cell holds a sorted list of positions in the gathered set.
#[derive(Default)]
struct PieceGrid {
    origin: P,
    cell: i64,
    nx: usize,
    ny: usize,
    /// Cell `i` occupies `starts[i]..starts[i + 1]` in `positions`.
    starts: Vec<u32>,
    positions: Vec<u32>,
    /// Next offset to fill in each cell during the build.
    cursor: Vec<u32>,
    /// Sorted, unique positions for a call spanning several cells.
    scratch: Vec<u32>,
    built: bool,
}

impl PieceGrid {
    /// Runs of consecutive cells, one per row of the box clamped to the grid.
    fn rows(&self, area: BBox) -> impl ExactSizeIterator<Item = Range<usize>> + 'static {
        let nx = self.nx;
        let index = |p: Nm, origin: Nm, n: usize| {
            ((p as i64 - origin as i64) / self.cell).clamp(0, n as i64 - 1) as usize
        };
        let left = index(area.min.x, self.origin.x, nx);
        let right = index(area.max.x, self.origin.x, nx) + 1;
        let rows = if area.is_empty() { 0..0 } else {
            index(area.min.y, self.origin.y, self.ny)..index(area.max.y, self.origin.y, self.ny) + 1
        };
        rows.map(move |y| y * nx + left..y * nx + right)
    }

    fn build(&mut self, gathered: &Gathered, cell: i64) {
        self.origin = gathered.area.min;
        self.cell = cell.max(1);
        let width = gathered.area.width();
        let height = gathered.area.height();
        let limit = 4 * gathered.pieces.len() as i64;
        loop {
            let nx = width / self.cell + 1;
            let ny = height / self.cell + 1;
            if nx.saturating_mul(ny) <= limit {
                self.nx = nx as usize;
                self.ny = ny as usize;
                if self.fill(gathered) { break; }
            }
            self.cell *= 2;
        }
        self.built = true;
    }

    /// Count each cell, then fill its slice in gathered order. Coarsen if the offsets cannot fit.
    fn fill(&mut self, gathered: &Gathered) -> bool {
        let cells = self.nx * self.ny;
        self.starts.clear();
        self.starts.resize(cells + 1, 0);
        for (_, item, _) in &gathered.pieces {
            for row in self.rows(item.bbox) {
                for count in &mut self.starts[row.start + 1..row.end + 1] { *count += 1; }
            }
        }
        let mut total = 0u64;
        let max_positions = u64::from(u32::MAX).min((isize::MAX as usize / size_of::<u32>()) as u64);
        for end in &mut self.starts {
            total += u64::from(*end);
            if total > max_positions { return false; }
            *end = total as u32;
        }
        self.cursor.clear();
        self.cursor.extend_from_slice(&self.starts[..cells]);
        self.positions.resize(total as usize, 0);
        for (pos, (_, item, _)) in gathered.pieces.iter().enumerate() {
            for row in self.rows(item.bbox) {
                for cursor in &mut self.cursor[row] {
                    self.positions[*cursor as usize] = pos as u32;
                    *cursor += 1;
                }
            }
        }
        true
    }

    /// A single cell can be read directly from `positions`. Otherwise collect the rows in `scratch`.
    fn query(&mut self, area: BBox) -> Option<Range<usize>> {
        let mut rows = self.rows(area);
        let Some(first) = rows.next() else { return Some(0..0) };
        if first.len() == 1 && rows.len() == 0 {
            return Some(self.starts[first.start] as usize..self.starts[first.end] as usize);
        }
        self.scratch.clear();
        for row in std::iter::once(first).chain(rows) {
            self.scratch.extend_from_slice(&self.positions[self.starts[row.start] as usize..self.starts[row.end] as usize]);
        }
        self.scratch.sort_unstable();
        self.scratch.dedup();
        None
    }
}

/// Exact board checks with conservative hulls for a single head.
/// Arc and polygon-set gaps cannot be threaded: coincident nearest points report `Thread`.
pub struct WorldObstacles<'a> {
    world: &'a World<'a>,
    rules: &'a RuleMatrix,
    copper: Copper,
    w: Nm,
    cache: &'a mut HullCache,
    /// World pieces in the order their item indices were assigned.
    entries: Vec<Entry<'a>>,
    indices: BTreeMap<WRef, u32>,
    failed: bool,
    gathered: Option<Gathered<'a>>,
}

impl<'a> WorldObstacles<'a> {
    pub fn new(
        world: &'a World<'a>, rules: &'a RuleMatrix, layer: LayerId, net: NetId,
        class: ClassId, w: Nm, id: ItemId, cache: &'a mut HullCache,
    ) -> Self {
        Self {
            world, rules, w, cache,
            copper: Copper {
                item: ConnItem::Track { item: id, seg: 0 }, net: Some(net), class, layer,
                shape: Shape::Segment { a: P::ZERO, b: P::ZERO, w },
            },
            entries: Vec::new(), indices: BTreeMap::new(), failed: false, gathered: None,
        }
    }

    /// Gather the pieces along `points` with one world query over their box, grown as in `items`.
    /// A call reaching outside that box queries only the new strips and merges their pieces in
    /// world order. Each call selects pieces whose boxes meet its own grown area.
    pub fn gather(&mut self, points: &[P]) {
        let area = grow(BBox::of_points(points.iter().copied()), self.grow_by());
        let pieces = self.world.query_sorted(self.copper.layer, area).into_iter()
            .map(|(reference, item)| (reference, item, self.indices.get(&reference).copied()))
            .collect();
        self.cache.grid.built = false;
        self.gathered = Some(Gathered { area, pieces, calls: 0 });
    }

    fn grow_by(&self) -> i64 {
        reach(self.rules) as i64 + (self.w as i64 + 1) / 2 + 1
    }

    fn extend_gathered(&mut self, gathered: &mut Gathered<'a>, area: BBox) {
        let union = gathered.area.union(area);
        let strips = strips(union, gathered.area);
        let mut new = Vec::new();
        for (i, &strip) in strips.iter().enumerate() {
            if strip.is_empty() { continue; }
            self.world.query(self.copper.layer, strip, |reference, item| {
                if !item.bbox.intersects(&gathered.area) && !strips[..i].iter().any(|s| item.bbox.intersects(s)) {
                    new.push((reference, item, self.indices.get(&reference).copied()));
                }
                ControlFlow::Continue(())
            });
        }
        new.sort_unstable_by_key(|&(r, it, _)| query_key(r, it));
        // Merge backwards in the existing vector, leaving the ordered prefix and its slots in place.
        let pieces = &mut gathered.pieces;
        let mut old = pieces.len();
        pieces.extend_from_slice(&new);
        let mut write = pieces.len();
        for piece in new.into_iter().rev() {
            let key = query_key(piece.0, piece.1);
            while old > 0 {
                let prev = pieces[old - 1];
                if query_key(prev.0, prev.1) < key { break; }
                old -= 1;
                write -= 1;
                pieces[write] = prev;
            }
            write -= 1;
            pieces[write] = piece;
        }
        gathered.area = union;
        self.cache.grid.built = false;
    }

    /// A nearby shape could not be represented as an inflated hull.
    pub fn failed(&self) -> bool {
        self.failed
    }

    /// The world piece behind a part's `PartKey::item`.
    pub fn reference(&self, item: u32) -> Option<WRef> {
        self.entries.get(item as usize).map(|entry| entry.reference)
    }

    fn copper(&self, a: P, b: P) -> Copper {
        Copper { shape: Shape::Segment { a, b, w: self.w }, ..self.copper.clone() }
    }

    fn items(&mut self, area: BBox) -> Vec<u32> {
        let area = grow(area, self.grow_by());
        if let Some(mut gathered) = self.gathered.take() {
            if !gathered.area.contains_box(&area) {
                self.extend_gathered(&mut gathered, area);
            }
            let len = gathered.pieces.len();
            let found = if gathered.calls == 0 || len < INDEX_FROM || u32::try_from(len).is_err() {
                self.gathered_items(&mut gathered, area, 0..len, |_, i| i)
            } else {
                if !self.cache.grid.built {
                    let cell = i64::from(super::tile_size(self.w, self.rules)) + 2 * self.grow_by();
                    self.cache.grid.build(&gathered, cell);
                }
                if let Some(cell) = self.cache.grid.query(area) {
                    self.gathered_items(&mut gathered, area, cell, |grid, i| grid.positions[i] as usize)
                } else {
                    let scratch = std::mem::take(&mut self.cache.grid.scratch);
                    let found = self.gathered_items(&mut gathered, area, 0..scratch.len(), |_, i| scratch[i] as usize);
                    self.cache.grid.scratch = scratch;
                    found
                }
            };
            gathered.calls = gathered.calls.saturating_add(1);
            self.gathered = Some(gathered);
            return found;
        }
        let mut found = Vec::new();
        for (reference, item) in self.world.query_sorted(self.copper.layer, area) {
            let index = match self.indices.get(&reference) {
                Some(&index) => index,
                None => match self.entry(reference, item) {
                    Some(index) => index,
                    None => break,
                },
            };
            found.push(index);
        }
        found
    }

    /// Visit gathered positions in order. Borrow the grid just to read a position, before `entry`.
    fn gathered_items(
        &mut self, gathered: &mut Gathered<'a>, area: BBox, range: Range<usize>,
        position: impl Fn(&PieceGrid, usize) -> usize,
    ) -> Vec<u32> {
        let mut found = Vec::new();
        for i in range {
            let (reference, item, slot) = &mut gathered.pieces[position(&self.cache.grid, i)];
            if !item.bbox.intersects(&area) { continue; }
            let index = match *slot {
                Some(index) => index,
                None => match self.entry(*reference, *item) {
                    Some(index) => { *slot = Some(index); index }
                    None => break,
                },
            };
            found.push(index);
        }
        found
    }

    /// A new entry for world piece `reference`; `None` (and failed) past `u32` entries.
    fn entry(&mut self, reference: WRef, item: &'a RItem) -> Option<u32> {
        let Ok(index) = u32::try_from(self.entries.len()) else {
            self.failed = true;
            return None;
        };
        let need = required_to(self.rules, &self.copper, &item.obstacle(), self.copper.layer).gap;
        self.entries.push(Entry { reference, item, need, hulls: None });
        self.indices.insert(reference, index);
        Some(index)
    }
}

impl Obstacles for WorldObstacles<'_> {
    fn near_items(&mut self, area: BBox, out: &mut Vec<u32>) {
        for index in self.items(area) {
            let entry = &mut self.entries[index as usize];
            if entry.need <= 0 { continue; }
            if entry.hulls.is_none() {
                let Some(hulls) = self.cache.hulls(self.world, entry, self.copper.layer, self.w) else {
                    self.failed = true;
                    continue;
                };
                entry.hulls = Some(hulls);
            }
            out.push(index);
        }
    }

    fn parts(&self, item: u32) -> &[Hull] {
        self.entries.get(item as usize).and_then(|entry| entry.hulls.as_deref()).unwrap_or(&[])
    }

    fn leg_clear(&mut self, item: u32, a: P, b: P) -> bool {
        let Some(entry) = self.entries.get(item as usize) else { return false };
        check_to(self.rules, &self.copper(a, b), &entry.item.obstacle(), self.copper.layer).is_none()
    }

    fn leg_room(&mut self, item: u32, a: P, b: P) -> Nm {
        let Some(entry) = self.entries.get(item as usize) else { return Nm::MIN / 2 };
        let copper = self.copper(a, b);
        let obstacle = entry.item.obstacle();
        let need = required_to(self.rules, &copper, &obstacle, self.copper.layer).gap;
        if need == 0 { return Nm::MAX / 2; }
        clearance(&copper.shape, obstacle.shape()).saturating_sub(allowed_need(need))
    }

    fn leg_gap(&mut self, item: u32, a: P, b: P) -> (Nm, Nm) {
        let Some(entry) = self.entries.get(item as usize) else { return (0, Nm::MAX) };
        check_to(self.rules, &self.copper(a, b), &entry.item.obstacle(), self.copper.layer)
            .map_or((entry.need, entry.need), |fault| (fault.gap, fault.need.gap))
    }

    fn gap(&mut self, x: u32, y: u32) -> GapInfo {
        let missing = GapInfo { have: 0, need: 0, near_x: P::ZERO, near_y: P::ZERO, offset_x: 0 };
        let (Some(x), Some(y)) = (self.entries.get(x as usize), self.entries.get(y as usize)) else {
            return missing;
        };
        let (sx, sy) = (x.item.shape(), y.item.shape());
        let (near_x, near_y) = nearest_points(sx, sy).unwrap_or((P::ZERO, P::ZERO));
        let (allowed_x, allowed_y) = (allowed_need(x.need), allowed_need(y.need));
        GapInfo {
            have: clearance(sx, sy),
            need: allowed_x.saturating_add(self.w).saturating_add(allowed_y),
            near_x, near_y, offset_x: allowed_x.saturating_add(self.w / 2),
        }
    }

    fn end_clear(&mut self, p: P) -> Option<(u32, Nm, Nm)> {
        for index in self.items(BBox::point(p)) {
            let entry = &self.entries[index as usize];
            if let Some(fault) = check_to(self.rules, &self.copper(p, p), &entry.item.obstacle(), self.copper.layer) {
                return Some((index, fault.gap, fault.need.gap));
            }
        }
        None
    }

    fn covers(&mut self, p: P) -> bool {
        // Keep the raw need for merge decisions. The hulls only choose candidates.
        // The exact legality checks in `check_to` and `gap` take the allowance.
        self.items(BBox::point(p)).into_iter().any(|index| {
            let entry = &self.entries[index as usize];
            entry.need > 0 && hull::covers(entry.item.shape(), entry.need, self.w, p)
        })
    }
}

/// Largest possible requirement, including the 1 nm keepout exclusion.
pub(super) fn reach(rules: &RuleMatrix) -> Nm {
    rules.max_clearance().max(rules.hole_clearance()).max(rules.edge()).max(1)
}

pub(super) fn grow(area: BBox, by: i64) -> BBox {
    if area.is_empty() { return area; }
    let coord = |n: i64| n.clamp(Nm::MIN as i64, Nm::MAX as i64) as Nm;
    BBox::new(
        P::new(coord(area.min.x as i64 - by), coord(area.min.y as i64 - by)),
        P::new(coord(area.max.x as i64 + by), coord(area.max.y as i64 + by)),
    )
}

/// Reject invalid dimensions and unrepresentable inflation before entering the hull builder.
pub(super) fn checked_parts(shape: &Shape, need: Nm, w: Nm) -> Option<Vec<Hull>> {
    let valid = match shape {
        Shape::Circle { r, .. } => *r >= 0,
        Shape::Segment { w, .. } | Shape::Arc { w, .. } => *w >= 0,
        Shape::Rect { c, half, .. } | Shape::Oval { c, half, .. } => {
            let radius = half.x as i64 + half.y as i64;
            half.x >= 0 && half.y >= 0 && [c.x, c.y].into_iter().all(|n| {
                n as i64 - radius > Nm::MIN as i64 && (n as i64 + radius) < Nm::MAX as i64
            })
        }
        Shape::Poly(_) => true,
    };
    if !valid || w < 0 { return None; }
    if need <= 0 || shape.bbox().is_empty() { return Some(Vec::new()); }
    let b = shape.bbox();
    // Leave room for boundary subdivisions and arc sagittae as well as the final inflation.
    let margin = need as i64 + (w as i64 + 1) / 2 + b.width().max(b.height()) + 2;
    if [b.min.x, b.min.y].into_iter().any(|v| v as i64 - margin <= Nm::MIN as i64)
        || [b.max.x, b.max.y].into_iter().any(|v| v as i64 + margin >= Nm::MAX as i64)
    {
        return None;
    }
    Some(hull::hull_parts(shape, need, w))
}

/// Nearest edge points for point, segment and filled convex cores, rounded to nearest nm.
/// Arc and polygon-set cores have no thread construction in this tier.
pub fn nearest_points(a: &Shape, b: &Shape) -> Option<(P, P)> {
    type F = (f64, f64);
    fn sub(a: F, b: F) -> F { (a.0 - b.0, a.1 - b.1) }
    fn dot(a: F, b: F) -> f64 { a.0 * b.0 + a.1 * b.1 }
    fn cross(a: F, b: F) -> f64 { a.0 * b.1 - a.1 * b.0 }
    fn along(a: F, d: F, t: f64) -> F { (a.0 + t * d.0, a.1 + t * d.1) }
    fn project(p: F, a: F, b: F) -> F {
        let d = sub(b, a);
        let len = dot(d, d);
        along(a, d, if len == 0.0 { 0.0 } else { (dot(sub(p, a), d) / len).clamp(0.0, 1.0) })
    }
    fn vertices(core: Core<'_>) -> Option<(Vec<P>, bool)> {
        match core {
            Core::Point(p) => Some((vec![p], false)),
            Core::Seg(a, b) => Some((vec![a, b], false)),
            Core::Convex(points) => Some((points, true)),
            Core::Arc(..) | Core::Set(_) => None,
        }
    }
    fn contains(points: &[P], p: P) -> bool {
        points.len() >= 3 && points.iter().zip(points.iter().cycle().skip(1))
            .all(|(&a, &b)| fab_elec_core::geom::orient(a, b, p) >= 0)
    }
    fn edges(points: &[P]) -> impl Iterator<Item = (F, F)> + '_ {
        points.iter().zip(points.iter().cycle().skip(1)).map(|(a, b)| (a.f(), b.f()))
    }
    let (ac, ar) = a.core();
    let (bc, br) = b.core();
    let (a, afilled) = vertices(ac)?;
    let (b, bfilled) = vertices(bc)?;
    let mut best = (a.first()?.f(), b.first()?.f());
    let mut distance = f64::INFINITY;
    let mut offer = |p: F, q: F| {
        let d = sub(q, p);
        let d2 = dot(d, d);
        if d2 < distance { best = (p, q); distance = d2; }
    };
    for &p in &a { if bfilled && contains(&b, p) { offer(p.f(), p.f()); } }
    for &p in &b { if afilled && contains(&a, p) { offer(p.f(), p.f()); } }
    for (p, q) in edges(&a) {
        for (r, s) in edges(&b) {
            offer(p, project(p, r, s));
            offer(q, project(q, r, s));
            offer(project(r, p, q), r);
            offer(project(s, p, q), s);
            let (d, e, rp) = (sub(q, p), sub(s, r), sub(r, p));
            let den = cross(d, e);
            if den != 0.0 {
                let (t, u) = (cross(rp, e) / den, cross(rp, d) / den);
                if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
                    let at = along(p, d, t);
                    offer(at, at);
                }
            }
        }
    }
    let (mut p, mut q) = best;
    let d = sub(q, p);
    let length = d.0.hypot(d.1);
    if length > 0.0 {
        p = along(p, d, ar / length);
        q = along(q, d, -br / length);
    }
    let rounded = |p: F| P::new(p.0.round() as Nm, p.1.round() as Nm);
    Some((rounded(p), rounded(q)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::band::{Part, PartKey};
    use fab_elec_core::{Angle, PolySet};

    fn near_parts(obs: &mut WorldObstacles<'_>, area: BBox, out: &mut Vec<Part>) {
        let mut items = Vec::new();
        obs.near_items(area, &mut items);
        for item in items {
            out.extend(obs.parts(item).iter().map(|hull| Part {
                key: PartKey { item, part: hull.part }, hull: hull.clone(),
            }));
        }
    }

    #[test]
    fn nearest_edges_project_intersect_and_include_filled_convex_cores() {
        let point = |x, y| Shape::Circle { c: P::new(x, y), r: 0 };
        let circle = Shape::Circle { c: P::ZERO, r: 2 };
        let other = Shape::Circle { c: P::new(10, 0), r: 3 };
        assert_eq!(nearest_points(&circle, &other), Some((P::new(2, 0), P::new(7, 0))), "radii must move both core points to their facing edges");
        let segment = Shape::Segment { a: P::new(-10, 0), b: P::new(10, 0), w: 2 };
        assert_eq!(nearest_points(&point(3, 5), &segment), Some((P::new(3, 5), P::new(3, 1))), "point/segment projection must include the segment radius");
        assert_eq!(nearest_points(&segment, &point(3, 5)), Some((P::new(3, 1), P::new(3, 5))), "swapping shapes must swap their nearest edges");
        let crossing = Shape::Segment { a: P::new(0, -5), b: P::new(0, 5), w: 0 };
        assert_eq!(nearest_points(&segment, &crossing), Some((P::ZERO, P::ZERO)), "crossing cores must give coincident points");
        let rect = Shape::Rect { c: P::ZERO, half: P::new(4, 3), rot: Angle::ZERO, corner: 0 };
        assert_eq!(nearest_points(&point(1, 1), &rect), Some((P::new(1, 1), P::new(1, 1))), "a contained point belongs to the filled convex core");
        assert_eq!(nearest_points(&rect, &point(8, 1)), Some((P::new(4, 1), P::new(8, 1))), "convex edges must support point projection");
        let other_rect = Shape::Rect { c: P::new(10, 0), half: P::new(2, 1), rot: Angle::ZERO, corner: 0 };
        let (a, b) = nearest_points(&rect, &other_rect).expect("two convex cores have nearest edges");
        assert_eq!((a.x, b.x, a.y), (4, 8, b.y), "parallel convex edges must select an equally high nearest pair");
        let arc = Shape::Arc { a: P::new(-2, 0), mid: P::new(0, 2), b: P::new(2, 0), w: 0 };
        assert_eq!(nearest_points(&arc, &point(0, 5)), None, "arc gaps have no thread construction in this tier");
        assert_eq!(nearest_points(&Shape::Poly(PolySet::new()), &point(0, 5)), None, "polygon-set gaps have no thread construction in this tier");
    }

    #[test]
    fn invalid_dimensions_and_unrepresentable_hulls_are_refused() {
        assert!(checked_parts(&Shape::Circle { c: P::ZERO, r: -1 }, 1, 2).is_none(), "negative shape radii must not reach the hull builder");
        assert!(checked_parts(&Shape::Circle { c: P::new(Nm::MAX, 0), r: 1 }, 1, 2).is_none(), "inflation outside the coordinate range must not panic");
        assert!(checked_parts(&Shape::Circle { c: P::ZERO, r: 1 }, 1, -2).is_none(), "negative head widths must not reach the hull builder");
    }

    #[test]
    fn world_keys_are_stable_and_kept_hulls_are_never_stale() {
        use crate::head::via;
        use crate::verify::new_pieces;
        use crate::world::RBase;
        use fab_elec_board::drc::DRC_EPSILON;
        use fab_elec_board::{check_via_in_pad, required, resolve_rules, Anchor, Item, Obstacle, Origin, Track};
        use fab_elec_core::{mm, Path};

        let design = fab_elec_fixtures::ledctl();
        let board = design.units[0].board.as_deref().expect("the fixture has a board");
        // The outer resistor has a signal on pad 2 and foreign power copper on pad 1.
        let placed = board.placed.iter().find(|p| p.path == "lamps[7].r").expect("the outer resistor exists");
        let a = placed.footprint.pad("1").expect("the power pad exists");
        let b = placed.footprint.pad("2").expect("the signal pad exists");
        let net = placed.pad_nets[b.ix()].expect("the signal pad has a net");
        let foreign_net = placed.pad_nets[a.ix()].expect("the power pad has a net");
        let at = placed.pad_at(a).expect("the power pad has a position");
        let start = placed.pad_at(b).expect("the signal pad has a position");
        let class = board.nets[net.ix()].class;
        let rules = resolve_rules(board, None);
        let base = RBase::build(board);
        let mut world = World::new(&base);
        let mut cache = HullCache::default();
        let mut obs = WorldObstacles::new(&world, &rules, LayerId::F_CU, net, class, mm(0.25), ItemId(board.next_item), &mut cache);
        let mut first = Vec::new();
        near_parts(&mut obs, BBox::point(at), &mut first);
        assert!(!first.is_empty(), "a foreign pad must contribute hull parts");
        let mut second = Vec::new();
        near_parts(&mut obs, BBox::point(start), &mut second);
        second.clear();
        near_parts(&mut obs, BBox::point(at), &mut second);
        assert_eq!(first.iter().map(|p| p.key).collect::<Vec<_>>(), second.iter().map(|p| p.key).collect::<Vec<_>>(), "intervening queries must not change part identities");
        assert!(first.iter().all(|p| obs.entries[p.key.item as usize].need > 0), "passable same-net pieces must never contribute parts");
        assert!(obs.end_clear(start).is_none(), "same-net pad and track copper must allow the head start");
        let (index, have, need) = obs.end_clear(at).expect("the foreign pad must fail the endpoint check");
        assert!(have < need && !obs.leg_clear(index, at, at), "endpoint and leg checks must agree on the deficit");
        assert_eq!(obs.leg_gap(index, at, at), (have, need), "leg gaps must preserve the exact pair fault");

        // The output capacitor's two foreign pads face each other across its centre.
        let cap = board.placed.iter().find(|p| p.path == "reg.c_out").expect("the output capacitor exists");
        let mut pads = Vec::new();
        board.item_copper(cap.id, &mut pads);
        let [pad_x, pad_y] = pads.as_slice() else { panic!("the output capacitor has two front pads") };
        let have = clearance(&pad_x.shape, &pad_y.shape);
        let [nx, ny] = [pad_x, pad_y].map(|pad| required(&rules, &obs.copper, pad, LayerId::F_CU).gap);
        let w = have - nx - ny + 600;
        assert!(nx > DRC_EPSILON && ny > DRC_EPSILON && w > 0, "the pad neck must fit a head with an allowance on each side");
        drop(obs);
        let mut obs = WorldObstacles::new(&world, &rules, LayerId::F_CU, net, class, w, ItemId(board.next_item), &mut cache);
        let mut items = Vec::new();
        obs.near_items(BBox::point(cap.at), &mut items);
        let [x, y] = [pad_x, pad_y].map(|pad| items.iter().copied().find(|&i| {
            matches!(obs.entries[i as usize].item.obstacle(), Obstacle::Copper(c) if c.item == pad.item)
        }).expect("both pads must be near their midpoint"));
        let gap = obs.gap(x, y);
        assert!(gap.have >= gap.need, "a neck short of the raw sum by less than the two allowances must thread: {gap:?}");
        // Along a horizontal neck, thread's rounded displacement is the signed offset itself.
        let d = gap.near_y.sub(gap.near_x);
        assert!(d.x != 0 && d.y == 0, "the capacitor pads must face each other horizontally");
        let point = gap.near_x.add(P::new(d.x.signum() * gap.offset_x, 0));
        assert!(obs.leg_clear(x, point, point) && obs.leg_clear(y, point, point), "the thread point must clear both pads: {gap:?}");
        drop(obs);

        // Just inside the raw ring fence on the outer face of pad_x.
        let size = rules.class(class).via.unwrap_or_else(|| rules.via(class));
        let owner = ConnItem::Via(ItemId(board.next_item));
        let ring_at = |c| Copper { item: owner, net: Some(net), class, layer: LayerId::F_CU, shape: Shape::Circle { c, r: size.pad / 2 } };
        let pad = Obstacle::Copper(pad_x);
        let nr = required_to(&rules, &ring_at(P::ZERO), &pad, LayerId::F_CU).gap;
        assert!(nr > DRC_EPSILON, "the ring-to-pad rule must have an allowance");
        let bounds = pad_x.shape.bbox();
        let offset = size.pad / 2 + nr - 300;
        let c = P::new(if d.x > 0 { bounds.min.x - offset } else { bounds.max.x + offset }, bounds.center().y);
        let ring = ring_at(c);
        assert_eq!(clearance(&ring.shape, &pad_x.shape), nr - 300, "the ring is 300 nm inside the raw pad fence");
        assert!(check_to(&rules, &ring, &pad, LayerId::F_CU).is_none(), "DRC must accept the ring beside the pad");
        let span = base.layers();
        let rings: Vec<_> = span.iter().map(|&layer| (Copper { layer, ..ring.clone() }, size.pad)).collect();
        let probe = via::Probe { rings: &rings, drill: (owner, Some(net), size.drill), span, in_pad: false };
        let fences = via::exclusion(&world, &rules, board, &probe, BBox::point(c), &|| false).expect("the fixture via has fences");
        assert!(fences.iter().any(|f| f.hull.strictly_contains(c)), "the raw fence must contain the via centre");
        let r = reach(&rules).max(rules.max_via_drill_to_pad(board)).max(rules.hole_to_hole()) as i64 + size.pad as i64;
        assert!(matches!(via::nearest(c, &fences, r, &|| false), via::Found::At(at) if at == c), "a via DRC passes keeps its place");

        // The outer resistor's same-net pad isolates the drill's mask web from copper clearance.
        pads.clear();
        board.item_copper(placed.id, &mut pads);
        let signal = pads.iter().find(|p| p.net == Some(net) && p.layer == LayerId::F_CU).expect("the signal pad has front copper");
        let web = rules.via_drill_to_board_pad(board, placed.id, b);
        let bounds = signal.shape.bbox();
        let c = P::new(bounds.max.x + size.drill / 2 + web - 300, bounds.center().y);
        let ring = ring_at(c);
        let raw = required(&rules, &ring, signal, LayerId::F_CU).gap;
        assert!(clearance(&ring.shape, &signal.shape) >= raw, "the ring must keep the raw copper gap");
        let drill = Shape::Circle { c, r: size.drill / 2 };
        let fault = check_via_in_pad(board, &rules, Some(net), &drill, signal).expect("DRC must refuse a short mask web");
        assert_eq!((fault.gap, fault.need.gap), (web - 300, web), "the drill must be 300 nm inside the raw web");
        assert!(fault.gap >= allowed_need(web), "the clearance allowance would pass this drill");
        let fences = via::exclusion(&world, &rules, board, &probe, grow(BBox::point(c), r), &|| false).expect("the fixture via has fences");
        assert!(matches!(via::nearest(c, &fences, r, &|| false), via::Found::At(at) if at != c), "a via short of the mask web must move");
        // Allowed in pads, the via has no web to keep from its own net's pad: nothing else moves it.
        let in_pad = via::Probe { rings: &rings, drill: (owner, Some(net), size.drill), span, in_pad: true };
        let fences = via::exclusion(&world, &rules, board, &in_pad, grow(BBox::point(c), r), &|| false).expect("the fixture via has fences");
        assert!(matches!(via::nearest(c, &fences, r, &|| false), via::Found::At(at) if at == c), "the mask web alone must move the via");

        let cached = cache.parts.clone();
        let at = P::mm(44.0, 19.0);
        let track = Item::Track(Track {
            id: ItemId(board.next_item + 1), net: foreign_net, layer: LayerId::F_CU,
            w: mm(0.25), widths: None, path: Path::new(at).line(at.add(P::mm(0.5, 0.0))),
            ends: [Anchor::Free; 2], locked: false, origin: Origin::Authored,
        });
        for piece in new_pieces(board, &track) { world.add(piece); }
        let mut obs = WorldObstacles::new(&world, &rules, LayerId::F_CU, net, class, mm(0.25), ItemId(board.next_item), &mut cache);
        let mut parts = Vec::new();
        near_parts(&mut obs, BBox::point(at), &mut parts);
        assert!(parts.iter().any(|p| matches!(obs.entries[p.key.item as usize].reference, WRef::Over(_))), "overlay copper must be queried as an obstacle");
        drop(obs);
        assert_eq!(cache.parts, cached, "overlay hulls must not pollute the cross-sample base cache");

        // A moved base piece (a dragged part's pad) is its base piece's hulls, moved: exactly the
        // hulls of its moved shape, from the cache once its base piece's are there.
        let rid = *base.pieces(placed.id).iter()
            .find(|&&rid| matches!(&base.get(rid).expect("a piece").piece, crate::world::RPiece::Copper(c) if c.net == Some(foreign_net) && c.layer == LayerId::F_CU))
            .expect("the power pad's copper");
        let by = P::new(733_331, -411_117);
        let moved_shape = base.get(rid).expect("the pad piece").shape().translated(by);
        let mut moving = World::new(&base);
        moving.remove_item(placed.id);
        let r = moving.add_moved(rid, by).expect("the base piece exists");
        assert_eq!(moving.moved_from(r), Some((rid, by)), "the overlay says which base piece it moved, and how far");
        let mut obs = WorldObstacles::new(&moving, &rules, LayerId::F_CU, net, class, mm(0.25), ItemId(board.next_item), &mut cache);
        let mut parts = Vec::new();
        near_parts(&mut obs, moved_shape.bbox(), &mut parts);
        let index = obs.indices[&r];
        let need = obs.entries[index as usize].need;
        let got: Vec<Hull> = parts.iter().filter(|p| p.key.item == index).map(|p| p.hull.clone()).collect();
        assert!(!got.is_empty() && got == checked_parts(&moved_shape, need, mm(0.25)).expect("a pad has hulls"),
            "a moved piece's hulls are those of its moved shape");
        drop(obs);
        let built = cache.built();
        let mut obs = WorldObstacles::new(&moving, &rules, LayerId::F_CU, net, class, mm(0.25), ItemId(board.next_item), &mut cache);
        near_parts(&mut obs, moved_shape.bbox(), &mut Vec::new());
        drop(obs);
        assert_eq!(cache.built(), built, "the moved piece's hulls come from the cache the second time");

        // An added piece's hulls are kept by what it is. A wave's world rebuilt with that piece
        // changed (a roll back lays another track there, at the same overlay place) gets the changed
        // piece's own hulls, never the kept ones; the piece as it was is found again.
        let laid = new_pieces(board, &track).into_iter().next().expect("the track has a piece");
        let mut changed = laid.clone();
        if let crate::world::RPiece::Copper(c) = &mut changed.piece { c.shape = c.shape.translated(P::new(0, 120_000)); }
        changed.bbox = changed.shape().bbox();
        let hulls_of = |piece: &crate::world::RItem, cache: &mut HullCache| -> Vec<Hull> {
            let mut over = World::new(&base);
            let r = over.add(piece.clone());
            let mut obs = WorldObstacles::new(&over, &rules, LayerId::F_CU, net, class, mm(0.25), ItemId(board.next_item), cache);
            let mut parts = Vec::new();
            near_parts(&mut obs, piece.bbox, &mut parts);
            let index = obs.indices[&r];
            let need = obs.entries[index as usize].need;
            let got: Vec<Hull> = parts.iter().filter(|p| p.key.item == index).map(|p| p.hull.clone()).collect();
            assert!(!got.is_empty() && got == checked_parts(piece.shape(), need, mm(0.25)).expect("a track has hulls"),
                "an added piece's hulls are those of the piece as it is now");
            got
        };
        // (The overlay queried above added this track: its hulls are kept until a newer sample.)
        cache.clear_added();
        let added = cache.built_added();
        let first = hulls_of(&laid, &mut cache);
        let second = hulls_of(&changed, &mut cache);
        assert!(first != second && cache.built_added() == added + 2, "a changed piece at the same place is built anew");
        assert!(hulls_of(&laid, &mut cache) == first && cache.built_added() == added + 2, "the piece as it was is found again");
        cache.clear_added();
        hulls_of(&laid, &mut cache);
        assert_eq!(cache.built_added(), added + 3, "a newer sample drops the added pieces' hulls");

        // A base piece taken out and put back is the base piece again, with its cached hulls.
        let mut back = World::new(&base);
        back.remove(rid);
        back.restore(rid);
        assert!(back.query_sorted(LayerId::F_CU, base.get(rid).expect("the pad piece").bbox).iter().any(|(r, _)| *r == WRef::Base(rid)),
            "a piece put back is the base piece itself");
        cache.clear();
        assert!(cache.parts.is_empty() && cache.added.is_empty(), "generation invalidation must release every cached hull");

        // Across the pad rows, compare the gathered positions with independent world queries.
        let head = board.tracks.first().expect("the fixture has a track");
        let class = board.nets[head.net.ix()].class;
        let mut direct_cache = HullCache::default();
        let mut direct = WorldObstacles::new(&world, &rules, head.layer, head.net, class, head.w, head.id, &mut direct_cache);
        let mut indexed = WorldObstacles::new(&world, &rules, head.layer, head.net, class, head.w, head.id, &mut cache);
        indexed.gather(&[P::ZERO, P::mm(46.0, 28.0)]);
        assert!(indexed.gathered.as_ref().expect("the reference gathered a set").pieces.len() >= INDEX_FROM,
            "the gathered reference must contain enough pieces to use the grid");
        let compare = |gathered: &mut WorldObstacles<'_>, direct: &mut WorldObstacles<'_>, area| {
            let (mut actual, mut expected) = (Vec::new(), Vec::new());
            gathered.near_items(area, &mut actual);
            direct.near_items(area, &mut expected);
            assert_eq!(actual, expected, "gathered queries must return the world's item indices in order for {area:?}");
            for item in actual {
                assert_eq!(gathered.reference(item), direct.reference(item), "item {item} must name the same world piece for {area:?}");
            }
        };
        let tile = crate::head::tile_size(head.w, &rules);
        for area in [
            BBox::point(start), BBox::point(P::ZERO), BBox::new(cap.at, cap.at.add(P::new(tile, tile))),
            BBox::new(P::mm(20.0, 10.0), P::mm(44.0, 27.0)), BBox::point(at), BBox::point(start),
        ] {
            compare(&mut indexed, &mut direct, area);
        }
        assert!(indexed.cache.grid.built, "repeated queries over the dense reference must exercise the grid");

        // Start in the resistor row and extend left, right, down, up, then across a corner.
        let mut extended = WorldObstacles::new(&world, &rules, head.layer, head.net, class, head.w, head.id, &mut cache);
        let mut direct = WorldObstacles::new(&world, &rules, head.layer, head.net, class, head.w, head.id, &mut direct_cache);
        let middle = P::mm(34.0, 22.0);
        extended.gather(&[middle]);
        compare(&mut extended, &mut direct, BBox::point(middle));
        let mut count = extended.gathered.as_ref().unwrap().pieces.len();
        for area in [
            BBox::point(P::mm(28.0, 22.0)), BBox::point(P::mm(42.0, 22.0)),
            BBox::point(P::mm(35.0, 16.0)), BBox::point(P::mm(36.0, 26.5)),
            BBox::point(P::mm(22.0, 14.0)), board.outline_bbox(),
        ] {
            compare(&mut extended, &mut direct, area);
            let pieces = &extended.gathered.as_ref().unwrap().pieces;
            assert!(pieces.len() > count, "extending the gathered box to {area:?} must add pieces");
            count = pieces.len();
        }
        let pieces = &extended.gathered.as_ref().unwrap().pieces;
        assert!(pieces.windows(2).all(|p| query_key(p[0].0, p[0].1) < query_key(p[1].0, p[1].1)),
            "the extended set must contain each world piece once in query order");
    }
}
