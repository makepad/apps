//! Embed and tighten a reference trail without a routing world.
//!
//! L: each leg misses every open hull or passes the exact clearance callback. O: every leg is
//! horizontal, vertical or diagonal. S: a complete band and the stripped reference keep every
//! part centre on the same side of their shared s-to-e chord. S failures are counted, not repaired;
//! a blocked prefix has a different chord and cannot be subjected to that global S test.
//!
//! Every replacement uses the half-open even-odd swept polygon: swept parts enter its constraint
//! list, or (in tightening) reject the move. A circle drawn round a part is ignored by loop stripping;
//! a noncrossing spiral is retained. Stripping costs O(p²) per removal. A round sorts and constructs
//! convex boundaries in O(m log m), then queries tiled legs and swept boxes. Each query
//! deduplicates item indices before the band reads any hull. A tiled leg visits each item once
//! and its completed result is cached for the call. Each admitted part keeps its checked vertices.
//! Rounds pass part handles and compare saved member order keys. Selecting a pair of overlapping
//! parts in rule e can additionally cost O(m²). The final L check queries each leg's
//! own tiles, so it costs the band's length, not its box. The S check counts over the parts the
//! embedder met, in O(parts * points): the region between a band and its reference is the union
//! of the per-leg sweeps, every part a query returns is met, and each leg's tiles meet the parts
//! along the band.
//!
//! Each leg gets at most 2 * seen + 1 rounds and at most |J| misorder splits; there are at most
//! legs0 - 1 merges. Tightening gets initial_points² + 1 passes. Exhaustion, unrepresentable input,
//! an interior chain endpoint at s/e, and entry into a same-side part of another run give Unsettled.
//! The latter is the documented geometric limit, with the offending leg and part in the stats.

use super::hull::{Hull, NORMALS};
use super::octi;
use super::side::{self, Keep};
use crate::proposal::{BlockCause, Blocked};
use fab_elec_core::geom::orient;
use fab_elec_core::{BBox, Nm, P};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

/// Stable identity of one inflated part of an obstacle item.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct PartKey {
    /// Obstacle item identity understood by the callbacks.
    pub item: u32,
    /// Part identity within the item.
    pub part: u32,
}

/// An inflated octagon and its stable identity; its centre must be strictly inside it.
#[derive(Clone, Debug)]
pub struct Part {
    /// Identity used for deduplication, ordering and wraps.
    pub key: PartKey,
    /// Conservative inflated octagon.
    pub hull: Hull,
}

/// Exact gap and the construction data for an octilinear thread through it.
#[derive(Copy, Clone, Debug)]
pub struct GapInfo {
    /// Available edge-to-edge gap.
    pub have: Nm,
    /// Head width plus the allowed clearance (`allowed_need`) on each side.
    pub need: Nm,
    /// Nearest point on the first shape.
    pub near_x: P,
    /// Nearest point on the second shape.
    pub near_y: P,
    /// First shape's allowed clearance (`allowed_need`) plus w/2, rounded down for an odd width.
    pub offset_x: Nm,
}

/// World callbacks; exact tests include the head width and the item's clearance requirement.
pub trait Obstacles {
    /// Append every item that may have a part whose hull meets area. Supersets, duplicates
    /// and any order are allowed.
    fn near_items(&mut self, area: BBox, out: &mut Vec<u32>);
    /// All hulls of an item returned by `near_items` in this run, stable for the run
    /// and in strictly increasing `hull.part` order.
    fn parts(&self, item: u32) -> &[Hull];
    /// Whether a straight head centreline keeps its exact clearance from item.
    fn leg_clear(&mut self, item: u32, a: P, b: P) -> bool;
    /// Clearance of the straight head leg `a..b` beyond the least gap allowed for `item`.
    /// Nonnegative where `leg_clear` holds, negative where the leg is short. An item requiring
    /// no gap gives `Nm::MAX / 2`. Two rooms can be added in `i64` without overflow.
    fn leg_room(&mut self, item: u32, a: P, b: P) -> Nm;
    /// Exact (have, need) for a leg, used to report an endpoint-slack failure.
    fn leg_gap(&mut self, item: u32, a: P, b: P) -> (Nm, Nm);
    /// Gap between shapes and the nearest points needed to thread an overlapping pair of hulls.
    fn gap(&mut self, x: u32, y: u32) -> GapInfo;
    /// First item broken by the head at p, with its exact (have, need).
    fn end_clear(&mut self, p: P) -> Option<(u32, Nm, Nm)>;
    /// Whether p is in a forbidden filled interior not covered by boundary hulls.
    fn covers(&mut self, p: P) -> bool;
}

/// Trail and endpoints for a sample; tile must be positive.
pub struct BandInput<'a> {
    /// Fixed start.
    pub s: P,
    /// Requested end.
    pub e: P,
    /// Drawn trail, stripped before endpoint replacement.
    pub reference: &'a [P],
    /// Maximum length of a straight-leg query tile.
    pub tile: Nm,
}

/// Consecutive points coincident with vertices of a part, with zero-based visit numbering.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WrapSpan {
    /// Part being visited.
    pub key: PartKey,
    /// Visit number for this part, in path order.
    pub visit: u32,
    /// First point index, inclusive.
    pub first: usize,
    /// Last point index, inclusive.
    pub last: usize,
}

/// Counters for one attempted leg, including attempts later superseded by a merge.
#[derive(Clone, Debug, Default)]
pub struct LegStats {
    /// Number of rounds actually performed.
    pub rounds: usize,
    /// Number of distinct part keys met in this attempt, including a part refused on admission.
    pub seen: usize,
    /// Number of misorder splits.
    pub splits: usize,
}

/// Geometry of the most recent unsettled collision for the caller's log.
#[derive(Clone, Debug)]
pub struct LimitGeometry {
    /// Entering candidate leg.
    pub leg: [P; 2],
    /// Part entered, when the limit is a collision.
    pub part: Option<PartKey>,
    /// Entered hull, retained so logging does not depend on a later world query.
    pub hull: Option<Hull>,
    /// Constraint keys in the supporting run.
    pub run: Vec<PartKey>,
}

/// Bounded work counters, including every internal item query.
#[derive(Clone, Debug, Default)]
pub struct BandStats {
    /// Total rounds over all attempted legs.
    pub rounds: usize,
    /// Removed interior reference endpoints.
    pub merges: usize,
    /// Misorder splits over all attempted legs.
    pub splits: usize,
    /// Calls to Obstacles::near_items, including final checks and wrap rebuilding.
    pub queries: usize,
    /// Failed global side comparisons; the legal answer is retained.
    pub s_failures: usize,
    /// Per-attempt proof counters; rounds <= 2 * seen + 1 for each entry.
    pub legs: Vec<LegStats>,
    /// Tightening passes, independent of legalization rounds.
    pub tighten_passes: usize,
    /// Fixed pass limit for the most recent tighten call.
    pub tighten_bound: usize,
    /// Geometry explaining an Unsettled answer, when available.
    pub limit: Option<LimitGeometry>,
}

/// An octilinear legal centreline, or its legal prefix with a reason for stopping.
#[derive(Clone, Debug)]
pub struct Band {
    /// Starts at s, and ends at e unless blocked.
    pub points: Vec<P>,
    /// Visits to part vertices along the centreline.
    pub wraps: Vec<WrapSpan>,
    /// Failure, including its location and exact gap where applicable.
    pub blocked: Option<Blocked>,
    /// Work and diagnostic counters.
    pub stats: BandStats,
}

type Vertex = (P, Option<PartKey>);
type PartHandle = usize;

/// Distinct admitted handles met during a leg attempt, plus its first refused part.
struct Seen {
    /// Last attempt to mark each handle. Starting a leg need only advance the stamp.
    marked: Vec<usize>,
    attempt: usize,
    count: usize,
}

impl Seen {
    fn new() -> Self { Self { marked: Vec::new(), attempt: 1, count: 0 } }

    fn clear(&mut self) {
        if self.attempt == usize::MAX {
            self.marked.clear();
            self.attempt = 0;
        }
        self.attempt += 1;
        self.count = 0;
    }

    fn mark(&mut self, handle: PartHandle) {
        if handle >= self.marked.len() { self.marked.resize(handle + 1, 0); }
        if self.marked[handle] != self.attempt {
            self.marked[handle] = self.attempt;
            self.count += 1;
        }
    }
}

fn unsettled(at: P) -> Blocked {
    Blocked { at, cause: BlockCause::Unsettled, need: 0, have: 0 }
}

fn rounded(num: i128, den: i128) -> i128 {
    if num >= 0 { (num + den / 2) / den } else { -((-num + den / 2) / den) }
}

fn point(x: i128, y: i128) -> Option<P> {
    Some(P::new(Nm::try_from(x).ok()?, Nm::try_from(y).ok()?))
}

fn midpoint(a: P, b: P) -> P {
    P::new(rounded(a.x as i128 + b.x as i128, 2) as Nm,
        rounded(a.y as i128 + b.y as i128, 2) as Nm)
}

fn intersection(a: P, b: P, c: P, d: P) -> Option<P> {
    let (abx, aby) = (b.x as i128 - a.x as i128, b.y as i128 - a.y as i128);
    let (cdx, cdy) = (d.x as i128 - c.x as i128, d.y as i128 - c.y as i128);
    let (acx, acy) = (c.x as i128 - a.x as i128, c.y as i128 - a.y as i128);
    let mut den = abx * cdy - aby * cdx;
    if den == 0 {
        // Overlaps have many intersections: choose the first along the first segment.
        return [a, b, c, d].into_iter()
            .filter(|&p| octi::on_segment(a, b, p) && octi::on_segment(c, d, p))
            .min_by_key(|&p| a.dist2(p));
    }
    let mut t = acx * cdy - acy * cdx;
    let mut u = acx * aby - acy * abx;
    if den < 0 { den = -den; t = -t; u = -u; }
    if t < 0 || t > den || u < 0 || u > den { return None; }
    // Products are at most 98 bits for i32 coordinates.
    point(rounded(a.x as i128 * den + abx * t, den),
        rounded(a.y as i128 * den + aby * t, den))
}

/// Remove closed portions at nonadjacent crossings, including touches and collinear overlaps.
/// Intersections round to nearest nm (halves away from zero); noncrossing spirals are unchanged.
pub fn strip_loops(reference: &[P]) -> Vec<P> {
    let mut points = reference.to_vec();
    points.dedup();
    // A removal discards at least one vertex. This finite budget also covers rounded crossings.
    for _ in 0..reference.len() {
        let mut crossing = None;
        'pairs: for i in 0..points.len().saturating_sub(1) {
            for j in i + 2..points.len().saturating_sub(1) {
                if let Some(p) = intersection(points[i], points[i + 1], points[j], points[j + 1]) {
                    crossing = Some((i, j, p));
                    break 'pairs;
                }
            }
        }
        let Some((i, j, p)) = crossing else { break };
        points.splice(i + 1..=j, [p]);
        points.dedup();
    }
    points
}

fn reference(input: &BandInput<'_>) -> Vec<P> {
    let mut result = strip_loops(input.reference);
    if result.len() < 2 {
        result = vec![input.s, input.e];
    } else {
        result[0] = input.s;
        if let Some(last) = result.last_mut() { *last = input.e; }
    }
    result.dedup();
    result
}

fn polygon(a: &[P], b: &[P]) -> Vec<P> {
    a.iter().copied().chain(b.iter().rev().copied()).collect()
}

fn sorted_items(obs: &mut impl Obstacles, area: BBox, items: &mut Vec<u32>) {
    items.clear();
    obs.near_items(area, items);
    items.sort_unstable();
    items.dedup();
}

fn item_parts(obs: &impl Obstacles, item: u32) -> &[Hull] {
    let hulls = obs.parts(item);
    debug_assert!(hulls.windows(2).all(|pair| pair[0].part < pair[1].part));
    hulls
}

/// Parts whose centres have odd parity in A followed by reversed B (with shared endpoints).
/// Results are deduplicated and sorted by key, independently of callback order.
pub fn swept(a: &[P], b: &[P], obs: &mut impl Obstacles) -> Vec<Part> {
    let poly = polygon(a, b);
    let mut parts = Vec::new();
    if !poly.is_empty() {
        let mut items = Vec::new();
        sorted_items(obs, BBox::of_points(poly.iter().copied()), &mut items);
        for item in items {
            for hull in item_parts(obs, item) {
                if octi::in_polygon_odd(&poly, hull.c) {
                    parts.push(Part { key: PartKey { item, part: hull.part }, hull: hull.clone() });
                }
            }
        }
    }
    parts
}

/// A part in admission order, with the vertices checked on its first encounter.
struct StoredPart {
    part: Part,
    vertices: [P; 8],
}

struct KnownItem {
    /// Consecutive handles of the admitted prefix, in part order.
    handles: Range<PartHandle>,
    /// Centre of the first refused part, where a later visit must stop again.
    refused: Option<P>,
    /// Most recent tiled leg to meet this item (zero until its first tiled leg).
    met: usize,
}

impl KnownItem {
    fn admit(item: u32, hulls: &[Hull], store: &mut Vec<StoredPart>) -> Self {
        let first = store.len();
        let mut refused = None;
        for hull in hulls {
            let Some(vertices) = octi::vertices(hull) else {
                refused = Some(hull.c);
                break;
            };
            let part = Part { key: PartKey { item, part: hull.part }, hull: hull.clone() };
            store.push(StoredPart { part, vertices });
        }
        Self { handles: first..store.len(), refused, met: 0 }
    }
}

/// State of one band call. Handles index the part store and item order determines its visits.
struct Context<'a, O> {
    obs: &'a mut O,
    tile: Nm,
    s: P,
    e: P,
    stats: BandStats,
    parts: Vec<StoredPart>,
    /// One record per queried item, in item order even for sparse callback indices.
    known: BTreeMap<u32, KnownItem>,
    /// Item indices reused for every tile and box query.
    items: Vec<u32>,
    /// Stamp shared by all tiles of the current uncached leg.
    tile_stamp: usize,
    /// Each leg's tiled part handles in key order, indexed by its ends.
    legs: BTreeMap<(P, P), Vec<PartHandle>>,
    /// Whether `finish` counts S failures and builds the visits (`Band::wraps`).
    visits: bool,
}

/// The handle of a part entered by a candidate edge.
struct Hit {
    handle: PartHandle,
    edge: usize,
}

impl<O: Obstacles> Context<'_, O> {
    fn known_parts(&self) -> impl Iterator<Item = &StoredPart> {
        let parts = &self.parts;
        self.known.values().flat_map(move |item| &parts[item.handles.clone()])
    }

    /// Fill the reusable handle list in the callback result's sorted, deduplicated order.
    fn query(
        &mut self, area: BBox, seen: &mut Seen, handles: &mut Vec<PartHandle>,
    ) -> Result<(), Blocked> {
        handles.clear();
        self.append_query(area, 0, seen, handles)
    }

    /// Append a box's handles, skipping items already met on this tiled leg. Zero is a single box.
    fn append_query(
        &mut self, area: BBox, stamp: usize, seen: &mut Seen, handles: &mut Vec<PartHandle>,
    ) -> Result<(), Blocked> {
        self.stats.queries = self.stats.queries.saturating_add(1);
        sorted_items(self.obs, area, &mut self.items);
        for &item in &self.items {
            let known = self.known.entry(item).or_insert_with(|| {
                KnownItem::admit(item, item_parts(self.obs, item), &mut self.parts)
            });
            if stamp != 0 {
                if known.met == stamp { continue; }
                known.met = stamp;
            }
            for handle in known.handles.clone() {
                seen.mark(handle);
                handles.push(handle);
            }
            if let Some(at) = known.refused {
                // A refusal ends this attempt, so its one handle-less key counts exactly once.
                seen.count += 1;
                return Err(unsettled(at));
            }
        }
        Ok(())
    }

    /// The parts the tiles of leg `a`-`b` meet, by key; each tile is one query, and each part is
    /// admitted where a tile first meets it (in tile order, as the tiles' own queries would).
    fn tiles<'h>(
        &mut self, a: P, b: P, seen: &mut Seen, handles: &'h mut Vec<PartHandle>,
    ) -> Result<&'h [PartHandle], Blocked> {
        if self.tile <= 0 { return Err(unsettled(a)); }
        handles.clear();
        if let Some(parts) = self.legs.get(&(a, b)) {
            for &handle in parts { seen.mark(handle); }
            handles.extend_from_slice(parts);
            return Ok(handles);
        }
        let dx = b.x as i128 - a.x as i128;
        let dy = b.y as i128 - a.y as i128;
        // L1 subdivision bounds Euclidean length too. Boxes enclose the exact rational tiles,
        // not rounded endpoints, so a narrow part between integer tile endpoints is not lost.
        let n = ((dx.abs() + dy.abs() + self.tile as i128 - 1) / self.tile as i128).max(1);
        if self.tile_stamp == usize::MAX {
            for known in self.known.values_mut() { known.met = 0; }
            self.tile_stamp = 0;
        }
        self.tile_stamp += 1;
        for i in 0..n {
            let x0 = a.x as i128 * n + dx * i;
            let x1 = x0 + dx;
            let y0 = a.y as i128 * n + dy * i;
            let y1 = y0 + dy;
            let min = P::new(x0.min(x1).div_euclid(n) as Nm, y0.min(y1).div_euclid(n) as Nm);
            let max = P::new((-(-x0.max(x1)).div_euclid(n)) as Nm,
                (-(-y0.max(y1)).div_euclid(n)) as Nm);
            self.append_query(BBox::new(min, max), self.tile_stamp, seen, handles)?;
        }
        handles.sort_unstable_by_key(|&handle| self.parts[handle].part.key);
        self.legs.insert((a, b), handles.clone());
        Ok(handles)
    }

    fn sweep(
        &mut self, a: &[P], b: &[P], seen: &mut Seen, handles: &mut Vec<PartHandle>,
    ) -> Result<(), Blocked> {
        handles.clear();
        // Identical paths sweep nothing, in particular a long diagonal has no box query here.
        if a == b { return Ok(()); }
        let poly = polygon(a, b);
        if poly.is_empty() { return Ok(()); }
        self.query(BBox::of_points(poly.iter().copied()), seen, handles)?;
        handles.retain(|&handle| octi::in_polygon_odd(&poly, self.parts[handle].part.hull.c));
        Ok(())
    }

    fn hits(
        &mut self, path: &[P], seen: &mut Seen, handles: &mut Vec<PartHandle>,
    ) -> Result<Vec<Hit>, Blocked> {
        let mut hits = Vec::new();
        for (edge, pair) in path.windows(2).enumerate() {
            for &handle in self.tiles(pair[0], pair[1], seen, handles)? {
                let part = &self.parts[handle].part;
                let at = [self.s, self.e].into_iter().find(|&end| {
                    (pair[0] == end || pair[1] == end) && part.hull.strictly_contains(end)
                });
                if at.is_some() || octi::enters(&part.hull, pair[0], pair[1]).is_some() {
                    if !self.obs.leg_clear(part.key.item, pair[0], pair[1]) {
                        if let Some(at) = at {
                            let (have, need) = self.obs.leg_gap(part.key.item, pair[0], pair[1]);
                            if have >= need { return Err(unsettled(at)); }
                            return Err(Blocked { at, cause: BlockCause::End, need, have });
                        }
                        hits.push(Hit { handle, edge });
                    }
                }
            }
        }
        Ok(hits)
    }

    fn initial_members(
        &mut self, a: &[P], chord: [P; 2], seen: &mut Seen, swept: &mut Vec<PartHandle>,
    ) -> Result<Vec<Member>, Blocked> {
        let hits = self.hits(&chord, seen, swept)?;
        self.sweep(a, &chord, seen, swept)?;
        let mut j = Vec::new();
        add_members(&mut j, &hits, swept, a, &self.parts);
        Ok(j)
    }
}

/// A part constraint with its side and rational order along the original leg's chord.
struct Member {
    handle: PartHandle,
    key: PartKey,
    side: Keep,
    order: (i128, i128),
}

struct Run {
    members: Range<usize>,
    side: Keep,
    vertices: Vec<(PartKey, P)>,
    boundary: Vec<Vertex>,
}

#[derive(Clone, Default)]
struct Edge {
    run: Option<usize>,
    before: Option<PartKey>,
    after: Option<PartKey>,
}

struct Candidate {
    points: Vec<P>,
    edges: Vec<Edge>,
}

impl Candidate {
    fn new(p: P) -> Self { Self { points: vec![p], edges: Vec::new() } }

    fn append(&mut self, p: P, edge: &Edge) {
        if self.points.last().copied() != Some(p) {
            self.points.push(p);
            self.edges.push(edge.clone());
        }
    }

    fn elbow(&mut self, a: P, b: P, side: Option<Keep>, edge: Edge) {
        let corners = octi::corners(a, b);
        let corner = match side {
            Some(Keep::Left) if orient(a, b, corners[0]) > 0 => corners[1],
            Some(Keep::Right) if orient(a, b, corners[0]) < 0 => corners[1],
            _ => corners[0],
        };
        self.append(corner, &edge);
        self.append(b, &edge);
    }
}

fn local_side(a: &[P], c: P) -> Keep {
    // keep's collinear-chord case is precisely the nearest-segment fallback, without a flip.
    side::keep(a, c).unwrap_or(Keep::Left)
}

fn order(a: P, b: P, part: &Part) -> (i128, i128) {
    if let Some(entry) = octi::enters(&part.hull, a, b) { return entry; }
    let dx = b.x as i128 - a.x as i128;
    let dy = b.y as i128 - a.y as i128;
    let dot = dx * (part.hull.c.x as i128 - a.x as i128)
        + dy * (part.hull.c.y as i128 - a.y as i128);
    (dot, (dx * dx + dy * dy).max(1))
}

fn compare_members(x: &Member, y: &Member) -> Ordering {
    octi::rational_cmp(x.order, y.order).then_with(|| x.key.cmp(&y.key))
}

fn add_members(
    j: &mut Vec<Member>, hits: &[Hit], swept: &[PartHandle], a: &[P], parts: &[StoredPart],
) -> bool {
    let (Some(&start), Some(&end)) = (a.first(), a.last()) else { return false };
    let mut keys: BTreeSet<_> = j.iter().map(|member| member.key).collect();
    let handles = hits.iter().map(|hit| hit.handle).chain(swept.iter().copied());
    let mut new: Vec<_> = handles.filter_map(|handle| {
        let part = &parts[handle].part;
        keys.insert(part.key).then(|| Member { handle, key: part.key,
            side: local_side(a, part.hull.c), order: order(start, end, part) })
    }).collect();
    if new.is_empty() { return false; }
    new.sort_by(compare_members);
    // Merge in sorted arrivals without reordering any existing members after a split.
    // Even if old order is no longer sorted, every skipped old key was <= the next arrival.
    let mut old = std::mem::take(j).into_iter().peekable();
    for member in new {
        for _ in 0..old.len() {
            if old.peek().is_some_and(|next| compare_members(next, &member).is_gt()) { break; }
            if let Some(next) = old.next() { j.push(next); }
        }
        j.push(member);
    }
    j.extend(old);
    true
}

fn convex(points: impl IntoIterator<Item = Vertex>) -> Vec<Vertex> {
    let mut points: Vec<_> = points.into_iter().collect();
    points.sort_by_key(|&(p, key)| (p, key.is_none(), key));
    points.dedup_by_key(|point| point.0);
    if points.len() < 3 { return points; }
    let mut lower: Vec<Vertex> = Vec::new();
    let mut upper: Vec<Vertex> = Vec::new();
    for (&p, stack) in points.iter().map(|p| (p, false))
        .chain(points.iter().rev().map(|p| (p, true))) {
        let out = if stack { &mut upper } else { &mut lower };
        for _ in 0..out.len() {
            let n = out.len();
            if n < 2 || orient(out[n - 2].0, out[n - 1].0, p.0) > 0 { break; }
            out.pop();
        }
        out.push(p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

fn runs(j: &[Member], parts: &[StoredPart]) -> Vec<Run> {
    let mut result: Vec<Run> = Vec::new();
    for (index, member) in j.iter().enumerate() {
        if result.last().map(|run| run.side) != Some(member.side) {
            result.push(Run { members: index..index, side: member.side,
                vertices: Vec::new(), boundary: Vec::new() });
        }
        if let Some(run) = result.last_mut() {
            run.members.end = index + 1;
            run.vertices.extend(parts[member.handle].vertices.iter().map(|&p| (member.key, p)));
        }
    }
    for run in &mut result {
        run.boundary = convex(run.vertices.iter().map(|&(key, p)| (p, Some(key))));
    }
    result
}

fn interior_endpoint(p: P, other: P, run: &Run) -> bool {
    // Including the other endpoint matters: it can swallow p even if the run alone does not.
    let boundary = convex(run.boundary.iter().copied().chain(std::iter::once((other, None))));
    boundary.len() >= 3 && (0..boundary.len()).all(|i| {
        orient(boundary[i].0, boundary[(i + 1) % boundary.len()].0, p) > 0
    })
}

type Vector = (i128, i128);

fn cross(a: Vector, b: Vector) -> i128 { a.0 * b.1 - a.1 * b.0 }
fn dot(a: Vector, b: Vector) -> i128 { a.0 * b.0 + a.1 * b.1 }
fn negative(a: Vector) -> Vector { (-a.0, -a.1) }
fn delta(a: P, b: P) -> Vector { (b.x as i128 - a.x as i128, b.y as i128 - a.y as i128) }

fn angular_cmp(a: &Vector, b: &Vector) -> Ordering {
    let half = |v: Vector| v.1 < 0 || (v.1 == 0 && v.0 < 0);
    half(*a).cmp(&half(*b)).then_with(|| cross(*a, *b).cmp(&0).reverse())
}

fn support(poly: &[Vertex], direction: Vector, index: &mut usize) {
    // On a CCW polygon, maximizers advance CCW as direction rotates CCW. Across the
    // angular sweep below these finite scans advance at most one complete revolution.
    for _ in 0..poly.len().saturating_sub(1) {
        let next = (*index + 1) % poly.len();
        if dot(delta(poly[*index].0, poly[next].0), direction) < 0 { break; }
        *index = next;
    }
}

fn bitangent(x: &Run, y: &Run) -> Option<(Vertex, Vertex)> {
    if x.boundary.len() < 3 || y.boundary.len() < 3 { return None; }
    let mut normals = Vec::new();
    for (poly, sign) in [(&x.boundary, -1_i128), (&y.boundary, 1_i128)] {
        for i in 0..poly.len() {
            let v = delta(poly[i].0, poly[(i + 1) % poly.len()].0);
            normals.push((sign * v.1, -sign * v.0));
        }
    }
    normals.sort_by(angular_cmp);
    normals.dedup_by(|a, b| cross(*a, *b) == 0 && dot(*a, *b) > 0);
    let mut ix = 0;
    let mut iy = 0;
    for i in 0..normals.len() {
        let lo = normals[i];
        let hi = normals[(i + 1) % normals.len()];
        let middle = if cross(lo, hi) == 0 && dot(lo, hi) < 0 {
            (-lo.1, lo.0)
        } else { (lo.0 + hi.0, lo.1 + hi.1) };
        if i == 0 {
            ix = (0..x.boundary.len()).min_by_key(|&k| {
                dot((x.boundary[k].0.x as i128, x.boundary[k].0.y as i128), middle)
            })?;
            iy = (0..y.boundary.len()).max_by_key(|&k| {
                dot((y.boundary[k].0.x as i128, y.boundary[k].0.y as i128), middle)
            })?;
        }
        support(&x.boundary, negative(middle), &mut ix);
        support(&y.boundary, middle, &mut iy);
        let (a, b) = (x.boundary[ix], y.boundary[iy]);
        let d = delta(a.0, b.0);
        if d == (0, 0) { return Some((a, b)); }
        let n = if x.side == Keep::Left { (-d.1, d.0) } else { (d.1, -d.0) };
        // In this angular interval a minimizes n.x and b maximizes n.y. At this
        // root their supporting lines coincide, so this is the inner bitangent.
        if cross(lo, n) >= 0 && cross(n, hi) >= 0 {
            return Some((a, b));
        }
    }
    None
}

fn ceil_displacement(component: i128, offset: Nm, distance2: i128) -> i128 {
    let numerator = component.abs() * offset as i128;
    let (mut lo, mut hi) = (0, component.abs());
    // An i32 coordinate difference has at most 32 bits; this is a fixed arithmetic search.
    for _ in 0..33 {
        if lo == hi { break; }
        let mid = (lo + hi) / 2;
        if octi::products([mid, mid, distance2], [numerator, numerator, 1]).is_lt() {
            lo = mid + 1;
        } else { hi = mid; }
    }
    lo * component.signum()
}

/// Cut an octilinear leg between overlapping parts. A slanted leg that lacks clearance slides
/// along its normal when the neck has room, then both parts are checked at the new chord.
fn thread(
    x: &StoredPart, y: &StoredPart, a: P, b: P, obs: &mut impl Obstacles,
) -> Result<(Vertex, Vertex), Blocked> {
    let gap = obs.gap(x.part.key.item, y.part.key.item);
    if gap.have < gap.need {
        let xb = BBox::of_points(x.vertices.iter().copied());
        let yb = BBox::of_points(y.vertices.iter().copied());
        let overlap = BBox {
            min: P::new(xb.min.x.max(yb.min.x), xb.min.y.max(yb.min.y)),
            max: P::new(xb.max.x.min(yb.max.x), xb.max.y.min(yb.max.y)),
        };
        return Err(Blocked { at: overlap.center(), cause: BlockCause::Gap,
            need: gap.need, have: gap.have });
    }
    let v = delta(gap.near_x, gap.near_y);
    let distance2 = dot(v, v);
    if distance2 == 0 || gap.offset_x < 0 || (gap.offset_x as i128).pow(2) > distance2 {
        return Err(Blocked { at: gap.near_x, cause: BlockCause::Thread, need: 0, have: 0 });
    }
    let g = point(gap.near_x.x as i128 + ceil_displacement(v.0, gap.offset_x, distance2),
        gap.near_x.y as i128 + ceil_displacement(v.1, gap.offset_x, distance2))
        .ok_or_else(|| unsettled(gap.near_x))?;
    let perpendicular = (-v.1, v.0);
    let mut direction = (1, 0);
    let mut best: Option<(i128, i128)> = None;
    for (nx, ny) in NORMALS {
        let d = (nx as i128, ny as i128);
        let projection = dot(d, perpendicular);
        if projection < 0 { continue; }
        let norm = dot(d, d);
        if best.map_or(true, |(old, old_norm)| {
            octi::products([projection, projection, old_norm], [old, old, norm]).is_gt()
        }) {
            best = Some((projection, norm));
            direction = d;
        }
    }
    // Orient the fixed leg in the original leg's direction; angular ties use NORMALS order.
    if dot(direction, delta(a, b)) < 0 { direction = negative(direction); }
    let (f1, f2) = thread_chord(x, y, g, direction).ok_or_else(|| unsettled(g))?;
    if obs.leg_clear(x.part.key.item, f1, f2) && obs.leg_clear(y.part.key.item, f1, f2) {
        return Ok(((f1, None), (f2, None)));
    }
    let (f1, f2) = slid_chord(x, y, g, direction, v, obs)
        .ok_or(Blocked { at: g, cause: BlockCause::Thread, need: 0, have: 0 })?;
    Ok(((f1, None), (f2, None)))
}

/// The chord through `g` extending one grid step past both hulls in `direction`.
fn thread_chord(x: &StoredPart, y: &StoredPart, g: P, direction: Vector) -> Option<(P, P)> {
    let (mut lower, mut upper) = ((0, 1), (0, 1));
    for hull in [&x.part.hull, &y.part.hull] {
        let mut lo = None;
        let mut hi = None;
        let mut misses = false;
        for (k, &(nx, ny)) in NORMALS.iter().enumerate() {
            let n = (nx as i128, ny as i128);
            let room = hull.s[k] as i128 - dot(n, (g.x as i128, g.y as i128));
            let slope = dot(n, direction);
            if slope == 0 {
                if room < 0 { misses = true; break; }
            } else if slope > 0 {
                let bound = (room, slope);
                if hi.map_or(true, |old| octi::rational_cmp(bound, old).is_lt()) { hi = Some(bound); }
            } else {
                let bound = (-room, -slope);
                if lo.map_or(true, |old| octi::rational_cmp(bound, old).is_gt()) { lo = Some(bound); }
            }
        }
        if !misses {
            if let (Some(lo), Some(hi)) = (lo, hi) {
                if !octi::rational_cmp(lo, hi).is_gt() {
                    if octi::rational_cmp(lo, lower).is_lt() { lower = lo; }
                    if octi::rational_cmp(hi, upper).is_gt() { upper = hi; }
                }
            }
        }
    }
    let first = lower.0.div_euclid(lower.1) - 1;
    let last = -(-upper.0).div_euclid(upper.1) + 1;
    let f1 = point(g.x as i128 + first * direction.0, g.y as i128 + first * direction.1)?;
    let f2 = point(g.x as i128 + last * direction.0, g.y as i128 + last * direction.1)?;
    Some((f1, f2))
}

/// Measure both rooms across the full parts before sliding the chord away from the short side.
fn slid_chord(
    x: &StoredPart, y: &StoredPart, g: P, direction: Vector, v: Vector, obs: &mut impl Obstacles,
) -> Option<(P, P)> {
    let bounds = BBox::of_points(x.vertices.iter().chain(&y.vertices).copied());
    let (dx, dy) = delta(bounds.center(), g);
    let ext = i128::from(bounds.width()) + i128::from(bounds.height()) + dx.abs() + dy.abs() + 2;
    let shift = |d: Vector, k: i128| point(g.x as i128 + k * d.0, g.y as i128 + k * d.1);
    let a = shift(direction, -ext)?;
    let b = shift(direction, ext)?;
    let rx = i64::from(obs.leg_room(x.part.key.item, a, b));
    let ry = i64::from(obs.leg_room(y.part.key.item, a, b));
    if rx + ry < 0 || (rx >= 0 && ry >= 0) { return None; }

    let mut n = (-direction.1, direction.0);
    if dot(n, v) < 0 { n = negative(n); }
    let t = i128::from(if rx < 0 { -rx } else { ry });
    let k = if n.0 != 0 && n.1 != 0 {
        // A diagonal grid step moves the line by sqrt(2) nm. Round the step count up.
        let square = t * t;
        let floor = (square / 2).isqrt();
        floor + i128::from(2 * floor * floor < square)
    } else { t.abs() };
    let g2 = shift(n, t.signum() * k)?;
    let (f1, f2) = thread_chord(x, y, g2, direction)?;
    (obs.leg_clear(x.part.key.item, f1, f2) && obs.leg_clear(y.part.key.item, f1, f2)).then_some((f1, f2))
}

#[derive(Copy, Clone)]
enum End { Start, Finish }

enum BuildError {
    Blocked(Blocked, Vec<P>),
    Pocket(End, Vec<P>),
}

fn candidate<O: Obstacles>(
    a: P, b: P, j: &[Member], runs: &[Run], context: &mut Context<'_, O>,
) -> Result<Candidate, BuildError> {
    let mut result = Candidate::new(a);
    if runs.is_empty() {
        // BandInput has no posture selector: its documented default is StraightFirst.
        result.elbow(a, b, None, Edge::default());
        return Ok(result);
    }
    let mut start = (a, None);
    for (i, run) in runs.iter().enumerate() {
        let join = if let Some(next) = runs.get(i + 1) {
            if let Some((end, start)) = bitangent(run, next) {
                Some((end, start, false))
            } else {
                let mut overlap = None;
                'pairs: for x in &j[run.members.clone()] {
                    let x = &context.parts[x.handle];
                    for y in &j[next.members.clone()] {
                        let y = &context.parts[y.handle];
                        if octi::octagons_overlap(&x.part.hull, &y.part.hull) {
                            overlap = Some((x, y));
                            break 'pairs;
                        }
                    }
                }
                let Some((x, y)) = overlap else {
                    // Convex bridges can overlap although no two actual octagons do. Rule e
                    // provides no physical pair in that case, so do not invent a gap oracle call.
                    return Err(BuildError::Blocked(unsettled(start.0), result.points));
                };
                let (end, start) = match thread(x, y, a, b, context.obs) {
                    Ok(join) => join,
                    Err(error) => return Err(BuildError::Blocked(error, result.points)),
                };
                Some((end, start, true))
            }
        } else { None };
        let end = join.map_or((b, None), |join| join.0);
        let Some(chain) = octi::chain(start.0, end.0, &run.vertices, run.side) else {
            if i == 0 && interior_endpoint(a, end.0, run) {
                return Err(BuildError::Pocket(End::Start, result.points));
            }
            if i + 1 == runs.len() && interior_endpoint(b, start.0, run) {
                return Err(BuildError::Pocket(End::Finish, result.points));
            }
            return Err(BuildError::Blocked(unsettled(start.0), result.points));
        };
        for pair in chain.windows(2) {
            result.elbow(pair[0].0, pair[1].0, Some(run.side), Edge {
                run: Some(i), before: pair[0].1, after: pair[1].1,
            });
        }
        if let Some((end, next_start, fixed)) = join {
            let left = Edge { run: Some(i), before: end.1, after: None };
            if fixed {
                result.append(next_start.0, &left);
            } else {
                let middle = midpoint(end.0, next_start.0);
                result.elbow(end.0, middle, Some(run.side), left);
                result.elbow(middle, next_start.0, Some(runs[i + 1].side), Edge {
                    run: Some(i + 1), before: None, after: next_start.1,
                });
            }
            start = next_start;
        }
    }
    Ok(result)
}

enum Solved {
    Path(Vec<P>),
    Pocket(End, Vec<P>),
    Blocked(Vec<P>, Blocked),
}

fn solve<O: Obstacles>(
    a: &[P], context: &mut Context<'_, O>, seen: &mut Seen, handles: &mut Vec<PartHandle>,
) -> Solved {
    let Some(&start) = a.first() else { return Solved::Blocked(vec![context.s], unsettled(context.s)) };
    let end = a.last().copied().unwrap_or(start);
    seen.clear();
    let mut counters = LegStats::default();
    let result = solve_leg(a, start, end, seen, &mut counters, context, handles);
    counters.seen = seen.count;
    context.stats.rounds = context.stats.rounds.saturating_add(counters.rounds);
    context.stats.splits = context.stats.splits.saturating_add(counters.splits);
    debug_assert!(counters.rounds <= counters.seen.saturating_mul(2).saturating_add(1));
    context.stats.legs.push(counters);
    result
}

fn solve_leg<O: Obstacles>(
    a: &[P], start: P, end: P, seen: &mut Seen, counters: &mut LegStats,
    context: &mut Context<'_, O>, swept: &mut Vec<PartHandle>,
) -> Solved {
    let mut j = match context.initial_members(a, [start, end], seen, swept) {
        Ok(j) => j,
        Err(blocked) => return Solved::Blocked(vec![start], blocked),
    };
    let mut prefix = vec![start];
    // The only open-ended legalization loop: every trip consumes the dynamic seen bound.
    loop {
        if counters.rounds >= seen.count.saturating_mul(2).saturating_add(1) {
            let at = prefix.last().copied().unwrap_or(start);
            return Solved::Blocked(prefix, unsettled(at));
        }
        counters.rounds += 1;
        let runs = runs(&j, &context.parts);
        let b = match candidate(start, end, &j, &runs, context) {
            Ok(b) => b,
            Err(BuildError::Pocket(end, prefix)) => return Solved::Pocket(end, prefix),
            Err(BuildError::Blocked(blocked, prefix)) => return Solved::Blocked(prefix, blocked),
        };
        let hits = match context.hits(&b.points, seen, swept) {
            Ok(hits) => hits,
            Err(blocked) => {
                // Endpoint exceptions can fail only on a leg incident to that endpoint.
                let last = b.points.windows(2).position(|pair| pair.contains(&blocked.at)).unwrap_or(0);
                return Solved::Blocked(b.points[..=last].to_vec(), blocked);
            }
        };
        prefix = match hits.first() {
            Some(hit) => b.points[..=hit.edge].to_vec(),
            None => b.points.clone(),
        };
        if let Err(blocked) = context.sweep(a, &b.points, seen, swept) {
            return Solved::Blocked(vec![start], blocked);
        }
        let old: BTreeSet<_> = j.iter().map(|member| member.key).collect();
        // Run ranges still refer to this snapshot while newly seen parts are being accumulated.
        let old_j: Vec<_> = j.iter().map(|member| member.key).collect();
        let added = add_members(&mut j, &hits, swept, a, &context.parts);
        let mut moved = false;
        for hit in &hits {
            let part = &context.parts[hit.handle].part;
            if !old.contains(&part.key) { continue; }
            let Some(edge) = b.edges.get(hit.edge) else { continue };
            let Some(run_index) = edge.run else { continue };
            let run = &runs[run_index];
            let Some(position) = j.iter().position(|m| m.handle == hit.handle) else { continue };
            if j[position].side == run.side || counters.splits >= j.len() {
                context.stats.limit = Some(LimitGeometry {
                    leg: [b.points[hit.edge], b.points[hit.edge + 1]], part: Some(part.key),
                    hull: Some(part.hull.clone()), run: old_j[run.members.clone()].to_vec(),
                });
                let first_bad = hits.first().map_or(hit.edge, |h| h.edge);
                return Solved::Blocked(b.points[..=first_bad].to_vec(), unsettled(b.points[hit.edge]));
            }
            // Insert between the chain's two supporting members, preserving all other order.
            let anchor = edge.after.or_else(|| {
                old_j.get(run.members.end.saturating_sub(1)).copied()
            });
            let at = if let Some(after) = edge.after {
                j.iter().position(|m| m.key == after)
            } else if let Some(before) = edge.before {
                j.iter().position(|m| m.key == before).map(|i| i + 1)
            } else {
                anchor.and_then(|key| j.iter().position(|m| m.key == key))
            };
            let Some(mut at) = at else {
                return Solved::Blocked(b.points[..=hit.edge].to_vec(), unsettled(b.points[hit.edge]));
            };
            if position < at { at -= 1; }
            if at == position {
                return Solved::Blocked(b.points[..=hit.edge].to_vec(), unsettled(b.points[hit.edge]));
            }
            let member = j.remove(position);
            j.insert(at, member);
            counters.splits += 1;
            moved = true;
            break;
        }
        if !added && !moved { return Solved::Path(b.points); }
    }
}

fn octilinear(a: P, b: P) -> bool {
    let (dx, dy) = delta(a, b);
    dx == 0 || dy == 0 || dx.abs() == dy.abs()
}

fn checked_len(points: &[P]) -> Option<(i64, i64)> {
    let (mut axis, mut diag) = (0_i64, 0_i64);
    for pair in points.windows(2) {
        let (dx, dy) = delta(pair[0], pair[1]);
        if dx.abs() == dy.abs() {
            diag = diag.checked_add(dx.abs() as i64)?;
        } else if dx == 0 || dy == 0 {
            axis = axis.checked_add(dx.abs().max(dy.abs()) as i64)?;
        } else { return None; }
    }
    Some((axis, diag))
}

fn wraps<'a>(points: &[P], known: impl Iterator<Item = &'a StoredPart>) -> Option<Vec<WrapSpan>> {
    let mut at: BTreeMap<P, BTreeSet<PartKey>> = BTreeMap::new();
    for stored in known {
        for &vertex in &stored.vertices {
            at.entry(vertex).or_default().insert(stored.part.key);
        }
    }
    let mut spans: Vec<WrapSpan> = Vec::new();
    let mut latest: BTreeMap<PartKey, usize> = BTreeMap::new();
    for (index, p) in points.iter().enumerate() {
        if let Some(keys) = at.get(p) {
            for &key in keys {
                let mut visit = 0;
                if let Some(&old) = latest.get(&key) {
                    if spans[old].last.checked_add(1) == Some(index) {
                        spans[old].last = index;
                        continue;
                    }
                    visit = spans[old].visit.checked_add(1)?;
                }
                latest.insert(key, spans.len());
                spans.push(WrapSpan { key, visit, first: index, last: index });
            }
        }
    }
    Some(spans)
}

/// Truncate at the first illegal leg, count S failures over the parts met, and rebuild the visits.
/// A part on the s-to-e chord line that no query met is outside the S count: there `side::keep`
/// falls back to its nearest segment, which says nothing about the region between the curves.
fn finish<O: Obstacles>(
    mut points: Vec<P>, mut blocked: Option<Blocked>, reference: &[P],
    mut context: Context<'_, O>,
) -> Band {
    if points.is_empty() { points.push(context.s); }
    points.dedup();
    // Check before asserting: malformed callback data must produce a Blocked, never a panic.
    let mut first_bad = None;
    let mut failed = None;
    let mut handles = Vec::new();
    let mut seen = Seen::new();
    for i in 0..points.len().saturating_sub(1) {
        let (a, b) = (points[i], points[i + 1]);
        if !octilinear(a, b) {
            first_bad = Some(i);
            break;
        }
        seen.clear();
        match context.tiles(a, b, &mut seen, &mut handles) {
            Ok(parts) => {
                if parts.iter().any(|&handle| {
                    let part = &context.parts[handle].part;
                    octi::enters(&part.hull, a, b).is_some() && !context.obs.leg_clear(part.key.item, a, b)
                }) {
                    first_bad = Some(i);
                    break;
                }
            }
            Err(error) => {
                failed = Some(error);
                break;
            }
        }
    }
    if let Some(error) = failed {
        blocked = Some(error);
        points.truncate(1);
    }
    if let Some(i) = first_bad {
        if blocked.is_none() { blocked = Some(unsettled(points[i])); }
        points.truncate(i + 1);
    }
    if checked_len(&points).is_none() {
        blocked = Some(unsettled(context.s));
        points.truncate(1);
    }
    debug_assert!(points.windows(2).all(|p| octilinear(p[0], p[1])), "band O");
    if blocked.is_none() && context.visits {
        let failures = context.known_parts().filter(|stored| {
            side::keep(&points, stored.part.hull.c) != side::keep(reference, stored.part.hull.c)
        }).count();
        context.stats.s_failures = context.stats.s_failures.saturating_add(failures);
        // The spec explicitly keeps a legal answer on S failure, including debug builds.
        debug_assert!(failures == 0 || context.stats.s_failures >= failures, "band S is reported");
    }
    let spans = match context.visits.then(|| wraps(&points, context.known_parts())) {
        Some(Some(spans)) => spans,
        Some(None) => { blocked = Some(unsettled(context.s)); Vec::new() }
        None => Vec::new(),
    };
    if let Some(error) = blocked.filter(|error| error.cause == BlockCause::Unsettled) {
        if context.stats.limit.is_none() {
            context.stats.limit = Some(LimitGeometry {
                leg: [error.at, error.at], part: None, hull: None, run: Vec::new(),
            });
        }
    }
    Band { points, wraps: spans, blocked, stats: context.stats }
}

fn append_path(points: &mut Vec<P>, path: &[P]) {
    for &p in path {
        if points.last().copied() != Some(p) { points.push(p); }
    }
}

/// Legalize the stripped trail with local sides, bounded closure, pockets and exact gap threads.
/// In the absence of a posture field in BandInput, free chords use StraightFirst.
pub fn make_legal(input: &BandInput<'_>, obs: &mut impl Obstacles) -> Band {
    legal(input, obs, true)
}

/// `make_legal` for a caller that reads only the points, `blocked` and the work counters: no S
/// count and no visits (`wraps` is empty). The answer is the same: every part met was checked to
/// have vertices when it was admitted, so the visits cannot fail.
pub fn make_legal_path(input: &BandInput<'_>, obs: &mut impl Obstacles) -> Band {
    legal(input, obs, false)
}

fn legal(input: &BandInput<'_>, obs: &mut impl Obstacles, visits: bool) -> Band {
    let reference = reference(input);
    let mut context = Context { obs, tile: input.tile, s: input.s, e: input.e,
        stats: BandStats::default(), parts: Vec::new(), known: BTreeMap::new(),
        items: Vec::new(), tile_stamp: 0, legs: BTreeMap::new(), visits };
    for end in [input.s, input.e] {
        if let Some((_, have, need)) = context.obs.end_clear(end) {
            let blocked = Blocked { at: end, cause: BlockCause::End, have, need };
            return finish(vec![input.s], Some(blocked), &reference, context);
        }
    }
    if input.tile <= 0 {
        return finish(vec![input.s], Some(unsettled(input.s)), &reference, context);
    }
    let legs0 = reference.len().saturating_sub(1);
    let mut legs: Vec<Vec<P>> = Vec::new();
    let mut a = vec![input.s];
    let mut queried = Vec::new();
    let mut seen = Seen::new();
    for (i, &p) in reference.iter().enumerate().skip(1) {
        a.push(p);
        if i + 1 != reference.len() {
            seen.clear();
            if let Err(blocked) = context.query(BBox::point(p), &mut seen, &mut queried) {
                return finish(vec![input.s], Some(blocked), &reference, context);
            }
            if context.obs.covers(p) || queried.iter().any(|&handle| {
                context.parts[handle].part.hull.strictly_contains(p)
            }) {
                context.stats.merges += 1;
                continue;
            }
        }
        legs.push(std::mem::replace(&mut a, vec![p]));
    }
    let mut finished: Vec<Vec<P>> = Vec::new();
    let mut blocked = None;
    let mut index = 0;
    // Each merge can revisit at most one finished leg. This finite traversal is in addition
    // to the explicit rounds/splits guard inside solve and the merge guard below.
    for _ in 0..legs0.saturating_mul(3).saturating_add(1) {
        if index == legs.len() { break; }
        match solve(&legs[index], &mut context, &mut seen, &mut queried) {
            Solved::Path(path) => { finished.push(path); index += 1; }
            Solved::Blocked(prefix, error) => { finished.push(prefix); blocked = Some(error); break; }
            Solved::Pocket(end, prefix) => {
                let merge_at = match end {
                    End::Start if index > 0 => Some(index - 1),
                    End::Finish if index + 1 < legs.len() => Some(index),
                    _ => None,
                };
                let Some(at) = merge_at.filter(|_| context.stats.merges < legs0.saturating_sub(1)) else {
                    let at = match end {
                        End::Start => legs[index].first().copied().unwrap_or(input.s),
                        End::Finish => legs[index].last().copied().unwrap_or(input.e),
                    };
                    blocked = Some(unsettled(at));
                    finished.push(prefix);
                    break;
                };
                let right = legs.remove(at + 1);
                legs[at].extend(right.into_iter().skip(1));
                if at < index { finished.pop(); }
                index = at;
                context.stats.merges += 1;
            }
        }
    }
    if blocked.is_none() && index != legs.len() { blocked = Some(unsettled(input.s)); }
    debug_assert!(context.stats.merges <= legs0.saturating_sub(1), "band merge bound");
    let mut points = vec![input.s];
    for path in finished { append_path(&mut points, &path); }
    finish(points, blocked, &reference, context)
}

/// Strictly shorten legal local subpaths without sweeping any part centre; rebuild visits.
/// Blocked bands are returned unchanged. Passes are bounded by the input point count squared + 1.
pub fn tighten(band: Band, input: &BandInput<'_>, obs: &mut impl Obstacles) -> Band {
    if band.blocked.is_some() { return band; }
    let reference = reference(input);
    let Band { mut points, stats, .. } = band;
    let mut context = Context { obs, tile: input.tile, s: input.s, e: input.e,
        stats, parts: Vec::new(), known: BTreeMap::new(), items: Vec::new(), tile_stamp: 0,
        legs: BTreeMap::new(), visits: true };
    if points.first().copied() != Some(input.s) || points.last().copied() != Some(input.e)
        || input.tile <= 0 {
        return finish(vec![input.s], Some(unsettled(input.s)), &reference, context);
    }
    let bound = match points.len().checked_mul(points.len()).and_then(|n| n.checked_add(1)) {
        Some(bound) => bound,
        None => return finish(points, Some(unsettled(input.s)), &reference, context),
    };
    context.stats.tighten_passes = 0;
    context.stats.tighten_bound = bound;
    let mut blocked = None;
    let mut settled = false;
    let mut swept = Vec::new();
    let mut seen = Seen::new();
    'passes: for _ in 0..bound {
        context.stats.tighten_passes += 1;
        let mut accepted = false;
        let mut index = 1;
        // Visit only points present at the beginning of this pass, in their original order.
        for _ in 0..points.len().saturating_sub(2) {
            if index + 1 >= points.len() { break; }
            let a = [points[index - 1], points[index], points[index + 1]];
            seen.clear();
            let j = match context.initial_members(&a, [a[0], a[2]], &mut seen, &mut swept) {
                Ok(j) => j,
                // An exact end failure of a proposed shortcut only rejects that shortcut.
                Err(error) if error.cause == BlockCause::End => { index += 1; continue; }
                Err(error) => { blocked = Some(error); break 'passes; }
            };
            if j.first().is_some_and(|first| j.iter().any(|member| member.side != first.side)) {
                index += 1;
                continue;
            }
            let runs = runs(&j, &context.parts);
            let b = match candidate(a[0], a[2], &j, &runs, &mut context) {
                Ok(b) => b.points,
                Err(BuildError::Pocket(_, _)) => { index += 1; continue; }
                Err(BuildError::Blocked(error, _)) => { blocked = Some(error); break 'passes; }
            };
            let (Some(old), Some(new)) = (checked_len(&a), checked_len(&b)) else {
                blocked = Some(unsettled(a[1])); break 'passes;
            };
            if !octi::cmp_len(new, old).is_lt() { index += 1; continue; }
            let clear = match context.hits(&b, &mut seen, &mut swept) {
                Ok(hits) => hits.is_empty(),
                Err(error) if error.cause == BlockCause::End => false,
                Err(error) => { blocked = Some(error); break 'passes; }
            };
            if !clear { index += 1; continue; }
            match context.sweep(&a, &b, &mut seen, &mut swept) {
                Ok(()) if swept.is_empty() => {}
                Ok(()) => { index += 1; continue; }
                Err(error) => { blocked = Some(error); break 'passes; }
            }
            let interior = b.len().saturating_sub(2);
            points.splice(index..index + 1, b.iter().skip(1).take(interior).copied());
            index += interior;
            accepted = true;
        }
        if !accepted { settled = true; break; }
    }
    if !settled && blocked.is_none() { blocked = Some(unsettled(input.s)); }
    finish(points, blocked, &reference, context)
}
