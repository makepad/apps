//! Place a via by clipping the leg against open hull interiors, without distance stepping.
//!
//! The via is through-hole: the stackup declares no other spans yet, and a via the board house
//! cannot drill is worse than a long one. It is tested on every copper layer, its drill against
//! every hole (hole to hole) and every pad (the drill-to-pad web, unless the class's vias may sit
//! in pads), in the answer's world: the committed base plus the gesture's earlier legs and vias.
//! It slides backward along its leg only, to the exact nearest free point.
//!
//! Cost: one query per copper layer over the leg's box, a hull build per obstacle met, then the
//! slide in O(hulls) per leg segment walked.

use super::obstacles::{checked_parts, grow, reach};
use crate::embed::hull::{Hull, NORMALS};
use crate::embed::octi::{enters, octagons_overlap, vertices};
use crate::world::{RPiece, World};
use fab_elec_board::{allowed_need, required_to, Board, ConnItem, Copper, Obstacle, RuleMatrix};
use fab_elec_core::dist::clearance;
use fab_elec_core::{BBox, ClassId, ItemId, LayerId, NetId, Nm, Shape, P};
use std::cell::Cell;
use std::collections::BTreeSet;

/// Nearest hull-free point back along the leg from its end, and its zero-based segment index
/// (0 for a one-point leg, which is tested where it is: a via at its start pad is via in pad).
/// `world` holds the committed base and what this answer laid before the leg.
pub fn place_via(
    leg: &[P], world: &World, rules: &RuleMatrix, board: &Board, net: NetId, class: ClassId, id: ItemId,
) -> Option<(P, usize)> {
    if leg.is_empty() { return None; }
    let size = rules.class(class).via.unwrap_or_else(|| rules.via(class));
    if size.pad <= 0 || size.drill <= 0 || size.drill > size.pad { return None; }
    let rings: Vec<(Copper, Nm)> = world.base.layers().iter().map(|&layer| {
        (Copper { item: ConnItem::Via(id), net: Some(net), class, layer, shape: Shape::Circle { c: P::ZERO, r: size.pad / 2 } }, size.pad)
    }).collect();
    let probe = Probe { rings: &rings, drill: (ConnItem::Via(id), Some(net), size.drill), span: world.base.layers(), in_pad: rules.via_in_pad(class) };
    let hulls: Vec<Hull> = exclusion(world, rules, board, &probe, BBox::of_points(leg.iter().copied()), &|| false)?.into_iter().map(|f| f.hull).collect();
    if let [p] = leg {
        return hulls.iter().all(|h| !h.strictly_contains(*p)).then_some((*p, 0));
    }
    slide(leg, &hulls)
}

/// A via as it keeps clear of what is round it: its ring on each layer that has one (its copper,
/// and the ring's diameter), its drill (owner, net, diameter) on the copper layers `span`, and
/// whether its class lets it sit in pads.
pub(super) struct Probe<'a> {
    pub(super) rings: &'a [(Copper, Nm)],
    pub(super) drill: (ConnItem, Option<NetId>, Nm),
    pub(super) span: &'a [LayerId],
    pub(super) in_pad: bool,
}

/// A hull in centre space, grown from the obstacle by the raw need and half the ring or drill
/// diameter. It only chooses candidates and can be wider by up to a twelfth where it rounds a
/// curve. `clear` judges each fence as DRC judges its rule.
pub(super) struct Fence<'w> {
    pub(super) hull: Hull,
    shape: &'w Shape,
    /// The smallest gap the obstacle's rule allows, as DRC judges it.
    least: Nm,
    w: Nm,
}

impl Fence<'_> {
    /// Whether the ring or drill centred at `p` keeps the least gap its rule allows.
    fn clear(&self, p: P) -> bool {
        clearance(&Shape::Circle { c: p, r: self.w / 2 }, self.shape) >= self.least
    }
}

/// Fence `shape` at the raw `need` for a ring or drill `w` across, with `least` the allowed gap.
/// `None` when the shape has no hull.
fn fenced<'w>(out: &mut Vec<Fence<'w>>, shape: &'w Shape, need: Nm, least: Nm, w: Nm) -> Option<()> {
    out.extend(checked_parts(shape, need, w)?.into_iter().map(|hull| Fence { hull, shape, least, w }));
    Some(())
}

/// The fences a via `probe` centred anywhere in `centres` keeps out of, against what `world` has
/// there: each ring against what is on its layer; existing copper against the drill (hole
/// clearance, as verify judges it); every pad against the drill (the drill-to-pad mask web, or
/// 1 nm from netless pads when it may sit in pads: filled and capped, as DRC judges it); and every
/// hole against the drill (hole to hole; holes spanning several layers once). `None` when a shape
/// has no hull. O(one query per span layer over `centres` grown by the reach, a hull build per
/// obstacle met).
pub(super) fn exclusion<'w>(world: &'w World, rules: &RuleMatrix, board: &Board, probe: &Probe, centres: BBox, gone: &dyn Fn() -> bool) -> Option<Vec<Fence<'w>>> {
    let (owner, net, drill_w) = probe.drill;
    let ring_w = probe.rings.iter().map(|&(_, w)| w).max().unwrap_or(0);
    let mut fences = Vec::new();
    let drill_shape = Shape::Circle { c: P::ZERO, r: drill_w / 2 };
    let drill = Obstacle::Drill { item: owner, net, shape: &drill_shape };
    let pad_reach = if probe.in_pad { 1 } else { rules.max_via_drill_to_pad(board) };
    let query = grow(centres, (reach(rules) as i64).max(pad_reach as i64) + (ring_w.max(drill_w) as i64 + 1) / 2 + 1);
    let mut pads = BTreeSet::new();
    for &layer in probe.span {
        let ring = probe.rings.iter().find(|(c, _)| c.layer == layer);
        for (reference, item) in world.query_sorted(layer, query) {
            if gone() { return None; }
            if let Some((ring, w)) = ring {
                let need = required_to(rules, ring, &item.obstacle(), layer).gap;
                if need > 0 { fenced(&mut fences, item.shape(), need, allowed_need(need), *w)?; }
            }
            // Existing copper also keeps hole clearance from the drill, as verify does.
            if let RPiece::Copper(existing) = &item.piece {
                let need = required_to(rules, existing, &drill, layer).gap;
                if need > 0 { fenced(&mut fences, item.shape(), need, allowed_need(need), drill_w)?; }
                if let ConnItem::Pad { placed, pad } = existing.item {
                    let need = match probe.in_pad {
                        true => Nm::from(existing.net.is_none()),
                        false => rules.via_drill_to_board_pad(board, placed, pad),
                    };
                    // DRC's via_pad_fault judges the mask web without the clearance allowance.
                    if need > 0 && pads.insert(reference) { fenced(&mut fences, item.shape(), need, need, drill_w)?; }
                }
            }
        }
    }
    // Drill spacing is independent of copper layers; deduplicate holes spanning several layers.
    let mut drills = BTreeSet::new();
    let need = rules.hole_to_hole();
    if need > 0 {
        let query = grow(centres, need as i64 + (drill_w as i64 + 1) / 2 + 1);
        for &layer in probe.span {
            for (reference, item) in world.query_sorted(layer, query) {
                if gone() { return None; }
                if let RPiece::Drill { shape, .. } = &item.piece {
                    if drills.insert(reference) { fenced(&mut fences, shape, need, allowed_need(need), drill_w)?; }
                }
            }
        }
    }
    Some(fences)
}

/// What `nearest` found.
pub(super) enum Found {
    /// The nearest free point.
    At(P),
    /// No free point.
    Nowhere,
    /// No free point within `r` of the centre: ask again with the fences to `r` (in each
    /// coordinate).
    Beyond(i64),
}

/// The nearest grid point to `c` that is clear of every fence, reached from `c` by a straight
/// move that crosses nothing it is clear of; `fences` are those of everything that can matter
/// within `r` of `c` (in each coordinate), so only a point within `r` of `c` can be told free.
/// A pushed via leaves what it is in, and never jumps over what is round it.
///
/// What it is in are the fences whose gap it really lacks at `c` (the pushers); a fence whose hull
/// holds `c` while its gap is kept there exactly (a hull rounding a curve is wider than its gap)
/// is one it stays clear of, exactly, and does not cross. `c` itself when it lacks no gap.
///
/// Shortening a move until it first meets a pusher's hull keeps it clear of every other hull, so
/// the nearest point is on a pusher's hull, which its move from `c` never leaves: only the hulls
/// that overlap that one can cut its boundary or shadow it from `c`. On each plane of its boundary
/// the free points are the plane less some open intervals: the interiors of those hulls, and
/// their shadows from `c`. Its nearest one is the foot of `c` or an interval's end: where another
/// hull's boundary crosses it, or where a ray from `c` through another hull's vertex grazes that
/// hull (`silhouette`). A hull inside another of its kind adds no interval and is left out
/// (`kinds`): one piece of copper fences several gaps, mostly nested. Those candidates within `r`, each rounded to the grid points on the plane either side of it, are
/// tried nearest first; candidates farther than the nearest free foot are not made, nor those of
/// hulls clear of the disc of radius `r`. None free within `r`: `Beyond` twice as far (to as far
/// as the pushers reach), or `Nowhere` once `r` covers them. So a via squeezed beside a long
/// pusher asks for the fences of its own neighbourhood first. O(pushers' planes x the hulls
/// overlapping them within `r`) candidates, a sort of them, and O(fences) per candidate tried.
pub(super) fn nearest(c: P, fences: &[Fence], r: i64, gone: &dyn Fn() -> bool) -> Found {
    let Some(kinds) = kinds(fences, c, gone) else { return Found::Nowhere };
    let pushers: Vec<&Hull> = fences.iter().zip(&kinds).filter(|(_, k)| **k == Kind::Pusher).map(|(f, _)| &f.hull).collect();
    if pushers.is_empty() {
        return Found::At(c);
    }
    // Every candidate is on a pusher's hull: past `reach` there are none.
    let (cx, cy) = (c.x as i64, c.y as i64);
    let reach = pushers.iter().map(|h| (h.s[0] - cx).max(h.s[4] + cx).max(h.s[2] - cy).max(h.s[6] + cy)).max().unwrap_or(0) + 2;
    let whole = reach <= r;
    // Only a candidate within `r` can be told free: the fences reach no farther.
    let lim = if whole { i128::MAX } else { (r as i128).pow(2) };
    let disc = BBox::new(P::new((cx - r) as Nm, (cy - r) as Nm), P::new((cx + r) as Nm, (cy + r) as Nm));
    let held: Vec<&Fence> = fences.iter().zip(&kinds).filter(|(_, k)| **k == Kind::Held).map(|(f, _)| f).collect();
    let outside: Vec<(&Hull, BBox)> = fences.iter().zip(&kinds).filter(|(_, k)| **k == Kind::Outside).map(|(f, _)| (&f.hull, hull_box(&f.hull))).collect();
    // Whether an outside hull keeps `p` out: it holds it, or the way there enters it.
    let blocks = |(h, b): &(&Hull, BBox), p: P, way: &BBox| b.intersects(way) && (h.strictly_contains(p) || enters(h, c, p).is_some());
    // The outside hull that last kept a candidate out is asked first: candidates near each other
    // are mostly kept out by the same one.
    let last = Cell::new(0);
    let free = |p: P| {
        let way = BBox::of_points([c, p]);
        if pushers.iter().any(|h| h.strictly_contains(p)) || outside.get(last.get()).is_some_and(|o| blocks(o, p, &way)) {
            return false;
        }
        if let Some(k) = outside.iter().position(|o| blocks(o, p, &way)) {
            last.set(k);
            return false;
        }
        held.iter().all(|f| f.clear(p) && clearance(&Shape::Segment { a: c, b: p, w: 0 }, f.shape) > 0)
    };
    // On hull h, its boundary included; `slack` takes a point rounded to the grid beside it.
    let on = |h: &Hull, p: P, slack: i64| NORMALS.iter().zip(h.s).all(|(&(nx, ny), s)| nx * p.x as i64 + ny * p.y as i64 <= s + slack);
    // The feet first: the nearest free one bounds the rest.
    let mut feet = Vec::new();
    for h in &pushers {
        if gone() { return Found::Nowhere; }
        for k in 0..8 {
            let plane = Plane::new(k, h.s[k]);
            let (ux, uy) = plane.u;
            plane.at(c, (c.x as i128 - plane.p0.0) * ux + (c.y as i128 - plane.p0.1) * uy, ux * ux + uy * uy, lim, |p| on(h, p, 0), &mut feet);
        }
    }
    sort(&mut feet);
    // `gone` (the search is not wanted any more) ends it with nothing found.
    let foot = feet.into_iter().find(|&(_, p)| gone() || free(p));
    if gone() { return Found::Nowhere; }
    let bound = foot.map_or(lim, |(d2, _)| d2);
    let (cx, cy) = (c.x as i128, c.y as i128);
    let mut ends = Vec::new();
    for h in &pushers {
        if gone() { return Found::Nowhere; }
        let near: Vec<&Hull> = fences.iter().zip(&kinds).filter(|(_, k)| **k != Kind::Covered).map(|(f, _)| &f.hull)
            .filter(|f| octagons_overlap(h, f) && (whole || hull_box(f).intersects(&disc))).collect();
        let corners: Vec<P> = near.iter().filter(|f| !f.strictly_contains(c)).flat_map(|f| silhouette(f, c)).collect();
        for k in 0..8 {
            let plane = Plane::new(k, h.s[k]);
            // A plane farther than the bound has no nearer candidate.
            if plane.off(c).pow(2) >= bound.saturating_mul(plane.nn()) { continue; }
            let (ux, uy) = plane.u;
            let (p0, uu) = (plane.p0, ux * ux + uy * uy);
            // Where the boundaries of the hulls overlapping it cross it (on both, to the grid).
            for f in &near {
                for (m, &(mx, my)) in NORMALS.iter().enumerate() {
                    let (mx, my) = (mx as i128, my as i128);
                    plane.at(c, f.s[m] as i128 - mx * p0.0 - my * p0.1, mx * ux + my * uy, bound, |p| on(h, p, 0) && on(f, p, 2), &mut ends);
                }
            }
            // Where the rays from c through their vertices cross it (the shadows' ends): the ray
            // c + l (v - c) meets n.p = s at l = (s - n.c) / n.(v - c), positive when it does
            // ahead (s - n.c is, c being inside), and t = (q - p0).u / u.u, all scaled by n.(v - c).
            let (nx, ny) = plane.n;
            let off = plane.s - nx * cx - ny * cy;
            for v in &corners {
                let (vx, vy) = (v.x as i128 - cx, v.y as i128 - cy);
                let den = nx * vx + ny * vy;
                if den <= 0 { continue; }
                let along = ((cx - p0.0) * den + off * vx) * ux + ((cy - p0.1) * den + off * vy) * uy;
                plane.at(c, along, den * uu, bound, |p| on(h, p, 0), &mut ends);
            }
        }
    }
    sort(&mut ends);
    let end = ends.into_iter().find(|&(_, p)| gone() || free(p));
    if gone() { return Found::Nowhere; }
    match end.or(foot) {
        Some((_, p)) => Found::At(p),
        None if whole => Found::Nowhere,
        None => Found::Beyond((2 * r).min(reach)),
    }
}

/// What a fence is to a via centred at `c`.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Kind {
    /// Its hull leaves `c` out: the way to a candidate may not enter it.
    Outside,
    /// Its hull holds `c` and `c` is not clear of it: a candidate is on or past its hull.
    Pusher,
    /// Its hull holds `c`, but `c` is clear of it exactly: a candidate keeps clear of it exactly.
    Held,
    /// An outside fence or a pusher inside another of its kind: that one keeps out all it would.
    Covered,
}

/// Each fence's kind for a via centred at `c`. An outside fence or a pusher whose hull lies within
/// the hull of another of the same kind is `Covered` (of equal hulls all but the first): it keeps
/// out no point and no way the other does not, and makes no candidate the other's do not. Only
/// the nests that are cheap to find are looked for: hulls round the same shape (the gaps of
/// several rules from one piece of copper), and equal hulls. `None` once the search is not wanted
/// (`gone`). O(fences log fences).
fn kinds(fences: &[Fence], c: P, gone: &dyn Fn() -> bool) -> Option<Vec<Kind>> {
    let mut kinds = Vec::with_capacity(fences.len());
    for f in fences {
        if gone() { return None; }
        kinds.push(match (f.hull.strictly_contains(c), f.clear(c)) {
            (false, _) => Kind::Outside,
            (true, false) => Kind::Pusher,
            (true, true) => Kind::Held,
        });
    }
    let within = |i: usize, j: usize| (0..8).all(|k| fences[i].hull.s[k] <= fences[j].hull.s[k]);
    // Fence `j` covers fence `i`: same kind, and `i` within it (of equal ones, the first covers).
    let covers = |kinds: &[Kind], j: usize, i: usize| j != i && kinds[j] == kinds[i] && within(i, j) && (j < i || !within(j, i));
    let nests: Vec<usize> = (0..fences.len()).filter(|&i| kinds[i] != Kind::Held).collect();
    let mut by_shape = nests.clone();
    by_shape.sort_unstable_by_key(|&i| (std::ptr::from_ref(fences[i].shape) as usize, i));
    let mut by_hull = nests;
    by_hull.sort_unstable_by_key(|&i| (fences[i].hull.s, i));
    let mut covered = vec![false; fences.len()];
    for run in by_shape.chunk_by(|&a, &b| std::ptr::eq(fences[a].shape, fences[b].shape)) {
        for &i in run {
            covered[i] |= run.iter().any(|&j| covers(&kinds, j, i));
        }
    }
    for run in by_hull.chunk_by(|&a, &b| fences[a].hull.s == fences[b].hull.s) {
        for &i in run {
            covered[i] |= run.iter().any(|&j| covers(&kinds, j, i));
        }
    }
    for (kind, covered) in kinds.iter_mut().zip(covered) {
        if covered { *kind = Kind::Covered; }
    }
    Some(kinds)
}

/// The vertices of hull `h` (outside it) whose rays from `c` graze it: the ends of the shadow it
/// casts, where one of the vertex's two edges faces `c` and the other does not (vertex `k` joins
/// edges `k` and `k + 1`). A ray through any other vertex goes on into the hull. Empty for a hull
/// with no vertices.
fn silhouette(h: &Hull, c: P) -> Vec<P> {
    let facing = |k: usize| NORMALS[k].0 * c.x as i64 + NORMALS[k].1 * c.y as i64 > h.s[k];
    let Some(vs) = vertices(h) else { return Vec::new() };
    (0..8).filter(|&k| facing(k) != facing((k + 1) % 8)).map(|k| vs[k]).collect()
}

/// The box of hull `h`.
fn hull_box(h: &Hull) -> BBox {
    BBox::new(P::new(-h.s[4] as Nm, -h.s[6] as Nm), P::new(h.s[0] as Nm, h.s[2] as Nm))
}

/// A hull plane n.p = s, and its grid points p0 + t u for every integer t (u is primitive).
struct Plane {
    n: (i128, i128),
    s: i128,
    p0: (i128, i128),
    u: (i128, i128),
}

impl Plane {
    fn new(k: usize, s: i64) -> Plane {
        let (nx, ny) = (NORMALS[k].0 as i128, NORMALS[k].1 as i128);
        let s = s as i128;
        let p0 = if nx != 0 { (s * nx, 0) } else { (0, s * ny) };
        Plane { n: (nx, ny), s, p0, u: (-ny, nx) }
    }

    /// n.n: 1 or 2.
    fn nn(&self) -> i128 {
        self.n.0 * self.n.0 + self.n.1 * self.n.1
    }

    /// s - n.c: the distance from c, times |n|.
    fn off(&self, c: P) -> i128 {
        self.s - self.n.0 * c.x as i128 - self.n.1 * c.y as i128
    }

    /// The grid points either side of t = num / den along the plane that `keep` keeps, with their
    /// squared distance from `c`, when it is under `bound`.
    fn at(&self, c: P, num: i128, den: i128, bound: i128, keep: impl Fn(P) -> bool, out: &mut Vec<(i128, P)>) {
        if den == 0 { return; }
        let (num, den) = if den < 0 { (-num, -den) } else { (num, den) };
        let lo = num.div_euclid(den);
        for t in [lo, lo + 1] {
            let (x, y) = (self.p0.0 + t * self.u.0, self.p0.1 + t * self.u.1);
            let (dx, dy) = (x - c.x as i128, y - c.y as i128);
            let d2 = dx * dx + dy * dy;
            if d2 >= bound { continue; }
            if let (Ok(px), Ok(py)) = (Nm::try_from(x), Nm::try_from(y)) {
                let p = P::new(px, py);
                if keep(p) {
                    out.push((d2, p));
                }
            }
        }
    }
}

/// Candidates nearest first, ties by position, once each.
fn sort(candidates: &mut Vec<(i128, P)>) {
    candidates.sort_unstable_by_key(|&(d2, p)| (d2, p.x, p.y));
    candidates.dedup();
}

/// Bounds are twice the step parameter: octilinear plane slopes are only 0, ±1 and ±2.
#[derive(Copy, Clone, Default)]
struct Interval {
    lo: i64,
    hi: i64,
}

fn clip(hull: &Hull, a: P, dx: i64, dy: i64) -> Option<Interval> {
    let mut interval = Interval { lo: i64::MIN, hi: i64::MAX };
    for (k, &(nx, ny)) in NORMALS.iter().enumerate() {
        let room = hull.s[k] - nx * a.x as i64 - ny * a.y as i64;
        let slope = nx * dx + ny * dy;
        if slope == 0 {
            if room <= 0 { return None; }
        } else if slope > 0 {
            interval.hi = interval.hi.min(2 * room / slope);
        } else {
            interval.lo = interval.lo.max(2 * room / slope);
        }
    }
    (interval.lo < interval.hi).then_some(interval)
}

/// Fixed-width radix ordering makes the backward interval scan O(k), including ordering.
fn order_by_exit(intervals: &mut Vec<Interval>) {
    let mut scratch = vec![Interval::default(); intervals.len()];
    for shift in (0..64).step_by(8) {
        let digit = |i: &Interval| (((i.hi as u64 ^ (1_u64 << 63)) >> shift) & 255) as usize;
        let mut counts = [0_usize; 256];
        for interval in intervals.iter() { counts[digit(interval)] += 1; }
        let mut next = 0;
        for count in &mut counts {
            let n = *count;
            *count = next;
            next += n;
        }
        for &interval in intervals.iter() {
            let bucket = &mut counts[digit(&interval)];
            scratch[*bucket] = interval;
            *bucket += 1;
        }
        std::mem::swap(intervals, &mut scratch);
    }
}

fn slide(leg: &[P], hulls: &[Hull]) -> Option<(P, usize)> {
    for (index, pair) in leg.windows(2).enumerate().rev() {
        let a = pair[0];
        let (x, y) = (pair[1].x as i64 - a.x as i64, pair[1].y as i64 - a.y as i64);
        if x != 0 && y != 0 && x.abs() != y.abs() { return None; }
        if x == 0 && y == 0 {
            if hulls.iter().all(|h| !h.strictly_contains(a)) { return Some((a, index)); }
            continue;
        }
        let (dx, dy) = (x.signum(), y.signum());
        let mut intervals: Vec<_> = hulls.iter().filter_map(|h| clip(h, a, dx, dy)).collect();
        order_by_exit(&mut intervals);
        let mut n = x.abs().max(y.abs());
        for interval in intervals.iter().rev() {
            if interval.lo < 2 * n && 2 * n < interval.hi {
                n = interval.lo.div_euclid(2);
                if n < 0 { break; }
            }
        }
        if n >= 0 {
            return Some((P::new((a.x as i64 + n * dx) as Nm, (a.y as i64 + n * dy) as Nm), index));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::hull::hull_parts;

    #[test]
    fn overlapping_intervals_slide_to_the_first_boundary_in_any_order() {
        let mut hulls = hull_parts(&Shape::Circle { c: P::new(14, 0), r: 2 }, 1, 2);
        hulls.extend(hull_parts(&Shape::Circle { c: P::new(8, 0), r: 2 }, 1, 2));
        let leg = [P::ZERO, P::new(16, 0)];
        assert_eq!(slide(&leg, &hulls), Some((P::new(4, 0), 0)), "overlapping exclusions form one backward slide");
        hulls.reverse();
        assert_eq!(slide(&leg, &hulls), Some((P::new(4, 0), 0)), "input hull order cannot change placement");
    }

    #[test]
    fn diagonal_fractional_entry_rounds_down_and_exhausted_segments_move_back() {
        let hull = Hull { s: [100, 100, 100, 100, 100, -5, 100, 100], c: P::new(4, 4), part: 0 };
        assert_eq!(slide(&[P::ZERO, P::new(8, 8)], &[hull.clone()]), Some((P::new(2, 2), 0)), "entry at 2.5 steps rounds down to step 2");
        assert_eq!(slide(&[P::ZERO, P::new(4, 4), P::new(8, 4)], &[hull]), Some((P::new(2, 2), 0)), "a fully blocked last segment resumes the preceding segment");
        let hulls = hull_parts(&Shape::Circle { c: P::ZERO, r: 20 }, 1, 2);
        assert_eq!(slide(&[P::ZERO, P::new(8, 0)], &hulls), None, "a wholly blocked leg has no via site");
    }
}
