//! The merge (R14 §5.3): the elements a layer's moves made on the layer below, merged where two
//! branches' areas overlap enough, in the reference's order.
//!
//! - The elements are put in the leaf order of a balanced box tree, cut into consecutive buckets
//!   of 4 (2 when there are few, at the reference's fixed 8 threads), merged inside each bucket,
//!   then bucket pairs merge round by round until one is left.
//! - The absorber is always the earlier element; bigger and smaller go by collision radius.
//! - What two elements merge into, or that they do not, is kept by their ids with the volume
//!   layers it read ([`Merges`]): a later run that tries the same two on volumes with those
//!   layers takes it as it is.
//! - What a group merges into is kept by its elements' ids in order ([`Groups`]): a later run
//!   with the same group takes it as it is, with the merges it tried, when it would take each of
//!   them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::Join;

use super::clip::{difference_ex, intersection_ex, union_ex};
use super::descent::{merged_id, Id};
use super::element::{moved_inside, simplified, some, Element, Move};
use super::offset::offset;
use super::settings::Settings;
use super::volumes::{reading, Kind, Reads, Volumes};
use crate::par::{Cancelled, Ctl};

/// What an element's box is its area's bounding box grown by (units): 100 nm.
const BOX_GROW: i64 = 100;
/// The thread count the buckets are cut for: the reference's, fixed (R14 §5.3 ruling).
const THREADS: usize = 8;
/// The grown part's round joins: chords within 0.01 mm of the arc.
const ARC: f64 = 0.01;
/// What the grown part is simplified at (mm).
const SIMPLIFY: f64 = 0.015;
/// The erosion an overlap must survive (mm): 25 µm, mitred to 1.2, which the engine takes as 2
/// (§1.5).
const SURVIVE: f64 = 0.025;
const SURVIVE_JOIN: Join = Join::Miter(1.2);

/// An element one layer down, with the parts of its area that get to the bed and to the model
/// (§5.1), which the merge tests.
#[derive(Clone, Debug, PartialEq)]
pub struct Stepped {
    pub e: Element,
    pub to_bed: Vec<ExPolygon>,
    pub to_model: Vec<ExPolygon>,
    /// It may merge this layer (§5.2): it moved, and not at the min gap inside the ramp.
    pub merges: bool,
}

/// An element as a step or a merge made it, with its box: its influence area's and parts'
/// bounding box grown by 100 nm. What it carries is not in it ([`Item`]), so one made the same
/// way in another run is the same, and is shared.
#[derive(Clone, Debug, PartialEq)]
pub struct Made {
    pub st: Stepped,
    pub bbox: BBox,
}

impl Made {
    /// `st` as made, its carries left to the [`Item`] that holds it.
    pub fn new(mut st: Stepped) -> Made {
        st.e.carries = Vec::new();
        let mut bbox = bbox_of(&st.e.area);
        bbox.merge(&bbox_of(&st.to_bed));
        bbox.merge(&bbox_of(&st.to_model));
        bbox.min = Pt::new(bbox.min.x - BOX_GROW, bbox.min.y - BOX_GROW);
        bbox.max = Pt::new(bbox.max.x + BOX_GROW, bbox.max.y + BOX_GROW);
        Made { st, bbox }
    }
}

/// An element made on the layer below: what it is, its indices on the layer above (the
/// elements it carries), and its id.
#[derive(Clone, Debug)]
pub struct Item {
    pub made: Arc<Made>,
    pub carries: Vec<usize>,
    pub id: Id,
}

impl Item {
    /// The element, with what it carries.
    pub fn element(&self) -> Element {
        Element { carries: self.carries.clone(), ..self.made.st.e.clone() }
    }
}

/// A merge tried: what the absorber and the absorbed merged into, with its id (none when they do
/// not merge), and the volume layers it read.
#[derive(Clone)]
pub struct Merged {
    made: Option<(Arc<Made>, Id)>,
    pub read: Reads,
}

/// The merges tried on one layer, by the ids of the absorber and the absorbed.
pub type Merges = HashMap<(Id, Id), Merged>;

/// What a group of elements merged into ([`merge`]): each element it ends with, as its made,
/// id, and the places in the group of the elements whose carries it takes, in order; and the
/// merges the group tried.
pub struct Grouped {
    out: Vec<(Arc<Made>, Id, Vec<usize>)>,
    tried: Merges,
}

/// What each group on a layer merged into, by the ids of its elements in their order.
pub type Groups = HashMap<Vec<Id>, Arc<Grouped>>;

pub struct Ctx<'a> {
    pub ctl: Ctl<'a>,
    pub s: &'a Settings,
    pub v: &'a Volumes,
    pub below: usize,
    /// The merges a run before tried on this layer.
    pub seen: &'a Merges,
    /// The merges this run tries, kept for the next.
    pub kept: &'a Mutex<Merges>,
    /// The run's number, for the ids it gives ([`merged_id`]).
    pub run: u64,
}

/// The elements made on layer `c.below`, merged: first those that do not merge, as they are,
/// then each group's merged ones in their final order ([`groups`]). A group `seen` holds from a
/// run before, by its elements' ids in order, is taken as it is when every merge it tried would
/// be ([`absorb`]); the others merge, each element's carries stood in for by its place in the
/// group. With the merged elements come this run's groups, for the next; every merge a group
/// tried goes into `c.kept`.
pub fn merge(c: &Ctx, made: Vec<Item>, seen: &Groups) -> Result<(Vec<Item>, Groups), Cancelled> {
    let ctl = c.ctl;
    let (items, mut out): (Vec<Item>, Vec<Item>) = made.into_iter().partition(|i| i.made.st.merges && (some(&i.made.st.to_bed) || some(&i.made.st.to_model)));
    let mut cut: Vec<(usize, Vec<Item>)> = Vec::new();
    let groups = groups(c.s, items);
    let n_groups = groups.len();
    let mut found: Vec<Option<Arc<Grouped>>> = vec![None; n_groups];
    let mut keys: Vec<Vec<Id>> = Vec::with_capacity(n_groups);
    let mut carried: Vec<Vec<Vec<usize>>> = Vec::with_capacity(n_groups);
    for (g, items) in groups.into_iter().enumerate() {
        let key: Vec<Id> = items.iter().map(|i| i.id).collect();
        found[g] = match seen.get(&key) {
            Some(grouped) if holds_all(c, &grouped.tried)? => Some(grouped.clone()),
            _ => None,
        };
        keys.push(key);
        let mut stand = Vec::with_capacity(if found[g].is_some() { 0 } else { items.len() });
        let mut carries = Vec::with_capacity(items.len());
        for (k, Item { made, carries: from, id }) in items.into_iter().enumerate() {
            carries.push(from);
            if found[g].is_none() {
                stand.push(Item { made, carries: vec![k], id });
            }
        }
        carried.push(carries);
        if found[g].is_some() {
            continue;
        }
        let mut items = stand;
        spatial_sort(&mut items);
        let size = if (items.len() + 2) / 4 >= THREADS { 4 } else { 2 };
        let mut it = items.into_iter();
        loop {
            let b: Vec<Item> = it.by_ref().take(size).collect();
            if b.is_empty() {
                break;
            }
            cut.push((g, b));
        }
    }
    // Every group's buckets, then its bucket pairs round by round, all groups side by side, each
    // keeping the merges it tries.
    let tried: Vec<Mutex<Merges>> = (0..n_groups).map(|_| Mutex::new(Merges::new())).collect();
    let at = |g: usize| Ctx { kept: &tried[g], ..*c };
    let mut buckets: Vec<Vec<Vec<Item>>> = (0..n_groups).map(|_| Vec::new()).collect();
    for done in crate::par::try_map_vec(ctl, cut, |_, (g, b)| within(&at(g), b).map(|b| (g, b)))? {
        let (g, b) = done?;
        buckets[g].push(b);
    }
    while buckets.iter().any(|b| b.len() > 1) {
        let mut pairs: Vec<(usize, Vec<Item>, Vec<Item>)> = Vec::new();
        let mut odd: Vec<Option<Vec<Item>>> = Vec::with_capacity(n_groups);
        for (g, b) in buckets.iter_mut().enumerate() {
            odd.push(if b.len() % 2 == 1 { b.pop() } else { None });
            let mut it = std::mem::take(b).into_iter();
            while let (Some(x), Some(y)) = (it.next(), it.next()) {
                pairs.push((g, x, y));
            }
        }
        for done in crate::par::try_map_vec(ctl, pairs, |_, (g, x, y)| across(&at(g), x, y).map(|b| (g, b)))? {
            let (g, b) = done?;
            buckets[g].push(b);
        }
        for (b, odd) in buckets.iter_mut().zip(odd) {
            b.extend(odd);
        }
    }
    let mut kept = Groups::with_capacity(n_groups);
    let mut all = c.kept.lock().unwrap_or_else(|e| e.into_inner());
    for (g, ((key, merged), tried)) in keys.into_iter().zip(buckets).zip(tried).enumerate() {
        let grouped = match found[g].take() {
            Some(grouped) => grouped,
            None => Arc::new(Grouped { out: merged.into_iter().flatten().map(|i| (i.made, i.id, i.carries)).collect(), tried: tried.into_inner().unwrap_or_else(|e| e.into_inner()) }),
        };
        all.extend(grouped.tried.iter().map(|(k, m)| (*k, m.clone())));
        out.extend(grouped.out.iter().map(|(made, id, from)| Item { made: made.clone(), carries: from.iter().flat_map(|&k| carried[g][k].iter().copied()).collect(), id: *id }));
        kept.insert(key, grouped);
    }
    Ok((out, kept))
}

/// Whether `c.v` has every volume layer each of `tried` read.
fn holds_all(c: &Ctx, tried: &Merges) -> Result<bool, Cancelled> {
    for m in tried.values() {
        if !c.v.holds(c.ctl, &m.read)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// `items` in groups that never meet: two elements share a group when they may merge by class
/// (on top or not, at the min gap or not) and box ([`near`]), or through others that do. Each
/// group keeps the order its elements came in, and the groups go by their first element. So
/// what one group merges into does not depend on the others, and a change in one leaves the
/// rest as they were.
fn groups(s: &Settings, items: Vec<Item>) -> Vec<Vec<Item>> {
    let n = items.len();
    if n == 0 {
        return Vec::new();
    }
    let drawn: Vec<f64> = items.iter().map(|i| i.made.st.e.width(s)).collect();
    let least = drawn.iter().copied().fold(f64::INFINITY, f64::min);
    // Two boxes `near` joins are no further apart than each one's width over the least,
    // summed: a sweep along x by boxes grown that much finds every pair to test.
    let reach: Vec<i64> = drawn.iter().map(|d| ((d - least) * SCALE) as i64 + 1).collect();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (items[i].made.bbox.min.x - reach[i], i));
    let mut root: Vec<usize> = (0..n).collect();
    fn find(root: &mut [usize], mut i: usize) -> usize {
        while root[i] != i {
            root[i] = root[root[i]];
            i = root[i];
        }
        i
    }
    for (k, &i) in order.iter().enumerate() {
        let a = &*items[i].made;
        let end = a.bbox.max.x + reach[i];
        for &j in &order[k + 1..] {
            let b = &*items[j].made;
            if b.bbox.min.x - reach[j] > end {
                break;
            }
            if a.st.e.on_top == b.st.e.on_top && a.st.e.min_gap == b.st.e.min_gap && near(s, a, b) {
                let (ri, rj) = (find(&mut root, i), find(&mut root, j));
                root[ri.max(rj)] = ri.min(rj);
            }
        }
    }
    let mut at: Vec<Option<usize>> = vec![None; n];
    let mut out: Vec<Vec<Item>> = Vec::new();
    for (i, item) in items.into_iter().enumerate() {
        let r = find(&mut root, i);
        let g = *at[r].get_or_insert_with(|| {
            out.push(Vec::new());
            out.len() - 1
        });
        out[g].push(item);
    }
    out
}

/// Whether `a`'s and `b`'s boxes meet once one is grown by the difference of their widths
/// (E5.1), the box test every merge passes.
fn near(s: &Settings, a: &Made, b: &Made) -> bool {
    let d = ((a.st.e.width(s) - b.st.e.width(s)).abs() * SCALE) as i64;
    let grown = BBox { min: Pt::new(a.bbox.min.x - d, a.bbox.min.y - d), max: Pt::new(a.bbox.max.x + d, a.bbox.max.y + d) };
    grown.overlaps(&b.bbox)
}

/// Inside a bucket: each element in turn absorbs the first later one it can; the bucket's last
/// takes the absorbed one's slot and the scan starts again after the absorber.
fn within(c: &Ctx, mut items: Vec<Item>) -> Result<Vec<Item>, Cancelled> {
    let mut i = 0;
    while i < items.len() {
        let mut j = i + 1;
        while j < items.len() {
            if let Some(m) = absorb(c, &items[i], &items[j])? {
                items[i] = m;
                items.swap_remove(j);
                j = i + 1;
            } else {
                j += 1;
            }
        }
        i += 1;
    }
    Ok(items)
}

/// A pair round: each element of `second` in turn is absorbed by the first element of `first`
/// that can; that element then absorbs what it can of the rest of `first` after it, the set's
/// last taking each absorbed one's slot and that slot tried again. What `second` keeps goes after
/// `first`, reordered as the reference reorders it: each absorbed one's slot takes the first of
/// those kept so far.
fn across(c: &Ctx, mut first: Vec<Item>, second: Vec<Item>) -> Result<Vec<Item>, Cancelled> {
    let mut second: Vec<Option<Item>> = second.into_iter().map(Some).collect();
    let mut begin = 0;
    for k in 0..second.len() {
        let Some(b) = second[k].as_ref() else { continue };
        let mut at = None;
        for i in 0..first.len() {
            if let Some(m) = absorb(c, &first[i], b)? {
                first[i] = m;
                at = Some(i);
                break;
            }
        }
        let Some(i) = at else { continue };
        second[k] = None;
        second.swap(k, begin);
        begin += 1;
        let mut j = i + 1;
        while j < first.len() {
            if let Some(m) = absorb(c, &first[i], &first[j])? {
                first[i] = m;
                first.swap_remove(j);
            } else {
                j += 1;
            }
        }
    }
    first.extend(second.into_iter().skip(begin).flatten());
    Ok(first)
}

/// `a` absorbing `b` (E5.1-E5.3), if the two may merge and their parts overlap enough. The
/// tests on their boxes and classes come first; what passes them is looked up by the two ids in
/// the merges seen before and taken where `c.v` has every volume layer it read, else worked
/// out ([`joined`]), and kept. Worked out the same as it was, it keeps its id.
fn absorb(c: &Ctx, a: &Item, b: &Item) -> Result<Option<Item>, Cancelled> {
    let s = c.s;
    let (ma, mb) = (&*a.made, &*b.made);
    let (ea, eb) = (&ma.st.e, &mb.st.e);
    if ea.on_top != eb.on_top || ea.min_gap != eb.min_gap {
        return Ok(None);
    }
    if !near(s, ma, mb) {
        return Ok(None);
    }
    // On a tie the absorbed one counts as the bigger.
    let (big, small) = if ea.collision_radius(s) > eb.collision_radius(s) { (ma, mb) } else { (mb, ma) };
    let mut gain = None;
    if ea.to_bed != eb.to_bed {
        let (bed, model) = if ea.to_bed { (ea, eb) } else { (eb, ea) };
        let (rb, rm) = (bed.drawn_radius(s), model.drawn_radius(s));
        if rb > rm {
            let g = model.merge_gain + (rb - rm);
            if g > s.model_merge_cap {
                return Ok(None);
            }
            gain = Some(g);
        }
    }
    let both_bed = ea.to_bed && eb.to_bed;
    if !both_bed && !ea.under_roof && !eb.under_roof && ea.depth.max(eb.depth) < s.min_height_on_model {
        return Ok(None);
    }
    if !big.st.e.hole_free && small.st.e.hole_free {
        return Ok(None);
    }
    let key = (a.id, b.id);
    let kept = c.seen.get(&key);
    let merged = match kept {
        Some(m) if c.v.holds(c.ctl, &m.read)? => m.clone(),
        _ => {
            let (made, reads) = reading(|| joined(c, ma, mb, gain));
            let made = made?;
            let read = c.v.now(c.ctl, reads)?;
            let made = made.map(|m| match kept.and_then(|k| k.made.as_ref()) {
                Some((was, id)) if **was == m => (was.clone(), *id),
                _ => (Arc::new(m), merged_id(a.id, b.id, c.run)),
            });
            Merged { made, read }
        }
    };
    c.kept.lock().unwrap_or_else(|e| e.into_inner()).insert(key, merged.clone());
    Ok(merged.made.map(|(made, id)| Item { made, carries: a.carries.iter().chain(&b.carries).copied().collect(), id }))
}

/// What `a` absorbing `b` makes, once they passed [`absorb`]'s tests (`gain`: the merge gain
/// the bed-bound one sets): none when their parts do not overlap enough.
fn joined(c: &Ctx, a: &Made, b: &Made, gain: Option<f64>) -> Result<Option<Made>, Cancelled> {
    let s = c.s;
    let (ea, eb) = (&a.st.e, &b.st.e);
    let (big, small) = if ea.collision_radius(s) > eb.collision_radius(s) { (a, b) } else { (b, a) };
    // Fab's: by the widths drawn ([`Element::width`]), not the counters' radii, so the smaller
    // one's branch reaches no further past the merged one's side than a move does.
    let diff = (big.st.e.width(s) - small.st.e.width(s)).abs();
    let both_bed = ea.to_bed && eb.to_bed;
    let both_min = ea.min_gap && eb.min_gap;
    // Fab's keep-out: the part's own collision only when both are leaving it, as the merged one
    // then is.
    let v = if ea.leaving() && eb.leaving() { c.v.free() } else { c.v };
    let collision = v.collision(c.ctl, c.below, s.rung_for(small.st.e.collision_radius(s), !both_min))?;
    let grow = |set: &[ExPolygon]| {
        if set.is_empty() {
            return Vec::new();
        }
        difference_ex(&simplified(&offset(set, diff * SCALE, Join::Round(ARC * SCALE)), SIMPLIFY), collision)
    };
    let (sp, bp) = if both_bed { (&small.st.to_bed, &big.st.to_bed) } else { (&small.st.to_model, &big.st.to_model) };
    let overlap = intersection_ex(&grow(sp), bp);
    if !some(&overlap) || !some(&offset(&overlap, -SURVIVE * SCALE, SURVIVE_JOIN)) {
        return Ok(None);
    }
    let mut n = ea.clone();
    n.depth = ea.depth.max(eb.depth);
    n.ramp = ea.ramp.max(eb.ramp);
    let r = ea.collision_radius(s).max(eb.collision_radius(s));
    // §5.3 quirk: the difference's absolute value, so a merged branch can come out wider than
    // both.
    n.foot = (r - s.radius(n.ramp, 0.0)).abs() / s.foot_step;
    n.to_bed = both_bed;
    n.on_top = ea.on_top && eb.on_top;
    n.under_roof = ea.under_roof || eb.under_roof;
    n.hold = ea.hold.max(eb.hold);
    n.roof_discs = ea.roof_discs.min(eb.roof_discs);
    n.hole_free = ea.hole_free || eb.hole_free;
    n.min_gap = ea.min_gap || eb.min_gap;
    // On a tie, the absorbed one's: the same layer.
    n.highest_tip = ea.highest_tip.max(eb.highest_tip);
    // Fab's: out of the keep-out by the higher of the two exits, at the merged radius, so only
    // where it still gets out by then ([`Volumes::leave_by`], as each step asks): a merge that
    // could not would end both branches in the air.
    n.exit = ea.exit.max(eb.exit);
    let overlap = if n.leaving() {
        let left = difference_ex(&overlap, &c.v.leave_by(c.ctl, n.rung(s), n.exit, c.below)?);
        if !some(&left) || !some(&offset(&left, -SURVIVE * SCALE, SURVIVE_JOIN)) {
            return Ok(None);
        }
        left
    } else {
        overlap
    };
    n.point = moved_inside(&overlap, ea.point);
    n.last = merged_move(ea.last, eb.last);
    n.merge_gain = gain.unwrap_or(ea.merge_gain.max(eb.merge_gain));
    n.carried = ea.carried.max(eb.carried);
    (n.placed, n.round, n.strayed, n.stuck) = (None, false, false, false);
    let mut area = intersection_ex(&grow(&small.st.e.area), &big.st.e.area);
    area.extend(overlap.iter().cloned());
    n.area = union_ex(&area);
    let (to_bed, to_model) = if both_bed {
        let model = if s.keys.bed_only { Vec::new() } else { intersection_ex(&grow(&small.st.to_model), &big.st.to_model) };
        (overlap, model)
    } else {
        (Vec::new(), overlap)
    };
    Ok(Some(Made::new(Stepped { e: n, to_bed, to_model, merges: true })))
}

/// The order of the avoidance kinds for a merged last move: Slow, then fast hole-free, then fast.
fn rank(k: Kind) -> usize {
    match k {
        Kind::Slow => 0,
        Kind::FastHoleFree => 1,
        Kind::Fast => 2,
    }
}

/// Two last moves merged (§5.3): the lower kind and speed; growth, error-free and moved from
/// either; the min gap only from both.
fn merged_move(a: Option<Move>, b: Option<Move>) -> Option<Move> {
    match (a, b) {
        (Some(a), Some(b)) => Some(Move {
            kind: if rank(a.kind) <= rank(b.kind) { a.kind } else { b.kind },
            speed: a.speed.min(b.speed),
            grow: a.grow || b.grow,
            may_stray: a.may_stray && b.may_stray,
            min_gap: a.min_gap && b.min_gap,
            moves: a.moves || b.moves,
        }),
        (a, b) => a.or(b),
    }
}

/// The elements in the leaf order of a balanced box tree (§5.3): each range is split at its
/// middle index by box centre along its combined box's longest side (x on a tie), into the
/// range up to the middle and the rest.
fn spatial_sort(items: &mut [Item]) {
    if items.len() < 2 {
        return;
    }
    let mut all = BBox::empty();
    for i in items.iter() {
        all.merge(&i.made.bbox);
    }
    let x = all.max.x - all.min.x >= all.max.y - all.min.y;
    let key = |i: &Item| {
        let b = &i.made.bbox;
        if x { (b.min.x + b.max.x) / 2 } else { (b.min.y + b.max.y) / 2 }
    };
    let mid = (items.len() - 1) / 2;
    select(items, mid, &key);
    let (a, b) = items.split_at_mut(mid + 1);
    spatial_sort(a);
    spatial_sort(b);
}

/// The reference's quick-select (§5.3): `items` reordered so that the `k`th is where a sort by
/// `key` puts it, those before it no larger and those after no smaller. Each round sorts the
/// range's ends and middle, parks the middle as the pivot next to the end, and partitions; equal
/// keys stop both scans.
fn select(items: &mut [Item], k: usize, key: &impl Fn(&Item) -> i64) {
    let (mut lo, mut hi) = (0, items.len() - 1);
    while lo < hi {
        let c = (lo + hi) / 2;
        for (a, b) in [(lo, c), (lo, hi), (c, hi)] {
            if key(&items[a]) > key(&items[b]) {
                items.swap(a, b);
            }
        }
        if hi <= lo + 2 {
            return;
        }
        let pivot = key(&items[c]);
        items.swap(c, hi - 1);
        let (mut i, mut j) = (lo, hi - 1);
        loop {
            i += 1;
            while key(&items[i]) < pivot {
                i += 1;
            }
            j -= 1;
            while key(&items[j]) > pivot && i < j {
                j -= 1;
            }
            if i >= j {
                break;
            }
            items.swap(i, j);
        }
        items.swap(i, hi - 1);
        if k < i {
            hi = i - 1;
        } else if k == i {
            return;
        } else {
            lo = i + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::element::disc;
    use super::super::settings::tests::r14;
    use super::super::settings::Keys;
    use super::*;

    fn bed_only() -> Settings {
        Settings::new(Keys { bed_only: true, ..r14(0.8).keys })
    }

    /// Element `id` one layer down on #5, to the bed, its area and to-bed part a disc of `r` mm
    /// at (`x`, `y`).
    fn item(s: &Settings, id: usize, x: f64, y: f64, r: f64) -> Item {
        let mut e = Element::tip(s, 5, Pt::from_mm(x, y));
        (e.to_bed, e.on_top, e.hole_free, e.min_gap, e.depth) = (true, true, true, false, 1);
        e.area = vec![disc(Pt::from_mm(x, y), r)];
        let made = Made::new(Stepped { to_bed: e.area.clone(), to_model: Vec::new(), merges: true, e });
        Item { made: Arc::new(made), carries: vec![id], id: id as Id }
    }

    fn ids(items: &[Item]) -> Vec<Vec<usize>> {
        items.iter().map(|i| i.carries.clone()).collect()
    }

    /// `item`'s element, to change before it merges.
    fn e(i: &mut Item) -> &mut Element {
        &mut Arc::get_mut(&mut i.made).unwrap().st.e
    }

    /// §5.3's quick-select on ties, traced by hand from its steps: seven boxes with one centre.
    /// The first split parks the pivot, swaps 1 with 4 and 2 with 5 (equal keys stop both scans)
    /// and leaves the pivot at the middle; the halves' selects then change nothing.
    #[test]
    fn the_spatial_sort_breaks_ties_as_the_reference_does() {
        let s = bed_only();
        let mut items: Vec<Item> = (0..7).map(|i| item(&s, i, 1.0, 1.0, 0.2)).collect();
        spatial_sort(&mut items);
        assert_eq!(ids(&items), vec![vec![0], vec![4], vec![5], vec![3], vec![1], vec![2], vec![6]]);
        // Distinct keys come out in order along the longer side, here y.
        let mut items: Vec<Item> = [3.0, 1.0, 4.0, 0.0, 2.0].iter().enumerate().map(|(i, &y)| item(&s, i, 0.0, y, 0.1)).collect();
        spatial_sort(&mut items);
        assert_eq!(ids(&items), vec![vec![3], vec![1], vec![4], vec![0], vec![2]]);
    }

    /// Groups join through a chain of boxes that meet and split by class; each keeps its
    /// elements' order and the groups go by their first element.
    #[test]
    fn groups_join_through_chains_and_split_by_class() {
        let s = bed_only();
        let mut items = vec![item(&s, 0, 5.0, 0.0, 0.3), item(&s, 1, 0.0, 0.0, 0.3), item(&s, 2, 1.0, 0.0, 0.3), item(&s, 3, 0.5, 0.0, 0.3), item(&s, 4, 0.5, 0.0, 0.3)];
        e(&mut items[4]).on_top = false;
        let g: Vec<Vec<Vec<usize>>> = groups(&s, items).iter().map(|g| ids(g)).collect();
        assert_eq!(g, vec![vec![vec![0]], vec![vec![1], vec![2], vec![3]], vec![vec![4]]]);
    }

    /// A group merges the same, ids included, whatever else is on the layer: here a far group put
    /// first and an element of another class on top of it.
    #[test]
    fn a_group_merges_the_same_whatever_else_is_on_the_layer() {
        let s = bed_only();
        let part = vec![Vec::new(); 8];
        let v = Volumes::new(Ctl::default(), &s, &part, &[]).unwrap();
        let (seen, kept) = (Merges::new(), Mutex::new(Merges::new()));
        let c = Ctx { ctl: Ctl::default(), s: &s, v: &v, below: 5, seen: &seen, kept: &kept, run: 1 };
        let row = |first: usize, x: f64| (0..4).map(|k| item(&s, first + k, x + 0.4 * k as f64, 0.0, 0.3)).collect::<Vec<_>>();
        let alone = merge(&c, row(0, 0.0), &Groups::new()).unwrap().0;
        let mut all = row(10, 20.0);
        all.extend(row(0, 0.0));
        let mut other = item(&s, 20, 0.4, 0.0, 0.3);
        e(&mut other).on_top = false;
        all.insert(1, other);
        let with = merge(&c, all, &Groups::new()).unwrap().0;
        let of = |items: &[Item]| items.iter().map(|i| (i.id, i.carries.clone())).collect::<Vec<_>>();
        assert!(alone.len() < 4, "the row merges");
        assert_eq!(of(&with[with.len() - alone.len()..]), of(&alone), "the row's merges come last, as they were alone");
        assert_eq!(with.len(), alone.len() * 2 + 1, "the far row merges as the row, the other class stays");
    }

    /// A group merged before is taken as it was, with the merges it tried, each element taking
    /// the carries of those it is made of as they are now; a group not seen before merges.
    #[test]
    fn a_group_merged_before_is_taken_as_it_was() {
        let s = bed_only();
        let part = vec![Vec::new(); 8];
        let v = Volumes::new(Ctl::default(), &s, &part, &[]).unwrap();
        let (seen, kept) = (Merges::new(), Mutex::new(Merges::new()));
        let c = Ctx { ctl: Ctl::default(), s: &s, v: &v, below: 5, seen: &seen, kept: &kept, run: 1 };
        let row = |first: usize, x: f64| (0..4).map(|k| item(&s, first + k, x + 0.4 * k as f64, 0.0, 0.3)).collect::<Vec<_>>();
        let mut all = row(0, 0.0);
        all.extend(row(10, 20.0));
        let (before, groups) = merge(&c, all.clone(), &Groups::new()).unwrap();
        let tried = std::mem::take(&mut *kept.lock().unwrap());
        assert!(before.len() < 8 && groups.len() == 2, "two groups, which merge");
        // The same elements carrying others, and the far row made otherwise (new ids).
        let mut again = all;
        for (k, i) in again.iter_mut().enumerate() {
            i.carries = vec![100 + k, 200 + k];
        }
        for i in &mut again[4..] {
            i.id += 1000;
        }
        let (after, _) = merge(&c, again.clone(), &groups).unwrap();
        let fresh = merge(&Ctx { kept: &Mutex::new(Merges::new()), ..c }, again, &Groups::new()).unwrap().0;
        let of = |items: &[Item]| items.iter().map(|i| (i.id, i.carries.clone())).collect::<Vec<_>>();
        assert_eq!(of(&after), of(&fresh), "taken or merged, the same elements with the same carries");
        let taken = after.iter().filter(|a| before.iter().any(|b| Arc::ptr_eq(&a.made, &b.made))).count();
        assert!(taken > 0 && taken < after.len(), "the row seen before is taken, the moved row merged");
        let now = kept.lock().unwrap();
        assert!(tried.keys().filter(|k| k.0 < 10).all(|k| now.contains_key(k)), "the merges the taken group tried are kept");
    }

    /// §5.3 pair round: the second set's A and B are kept, C is absorbed by the first set's X,
    /// D is kept. C's slot takes the first kept one, so they follow X as B, A, D. The absorber
    /// is X, the earlier element; its carries come first.
    #[test]
    fn a_pair_round_reorders_what_the_second_set_keeps() {
        let s = bed_only();
        let part = vec![Vec::new(); 8];
        let v = Volumes::new(Ctl::default(), &s, &part, &[]).unwrap();
        let (seen, kept) = (Merges::new(), Mutex::new(Merges::new()));
        let c = Ctx { ctl: Ctl::default(), s: &s, v: &v, below: 5, seen: &seen, kept: &kept, run: 1 };
        let first = vec![item(&s, 9, 0.0, 0.0, 0.3)];
        let second = vec![item(&s, 0, 5.0, 0.0, 0.3), item(&s, 1, 10.0, 0.0, 0.3), item(&s, 2, 0.2, 0.0, 0.3), item(&s, 3, 15.0, 0.0, 0.3)];
        let out = across(&c, first, second).unwrap();
        assert_eq!(ids(&out), vec![vec![9, 2], vec![1], vec![0], vec![3]]);
        let m = &out[0].made.st.e;
        assert!(m.placed.is_none() && m.to_bed && m.on_top, "a merged element has no placed point and keeps its class");
    }

    /// §5.3 "Which pairs may merge" and "The merged element": the refusals, the 25 µm erosion,
    /// and every field of the merged one.
    #[test]
    fn merges_follow_the_pair_rules() {
        let s = bed_only();
        let part = vec![Vec::new(); 8];
        let v = Volumes::new(Ctl::default(), &s, &part, &[]).unwrap();
        let (seen, kept) = (Merges::new(), Mutex::new(Merges::new()));
        let c = Ctx { ctl: Ctl::default(), s: &s, v: &v, below: 5, seen: &seen, kept: &kept, run: 1 };
        let pair = |f: &dyn Fn(&mut Element, &mut Element)| {
            let (mut a, mut b) = (item(&s, 0, 0.0, 0.0, 0.3), item(&s, 1, 0.4, 0.0, 0.3));
            f(e(&mut a), e(&mut b));
            absorb(&c, &a, &b).unwrap()
        };
        assert!(pair(&|_, _| ()).is_some(), "0.2 mm of overlap merges");
        assert!(pair(&|_, b| b.min_gap = true).is_none(), "never the min gap with the full side gap");
        assert!(pair(&|_, b| b.on_top = false).is_none(), "never on-top with not on-top");
        assert!(pair(&|a, b| (a.hole_free, b.ramp) = (true, 1)).is_some() && pair(&|a, b| (a.hole_free, b.hole_free, b.ramp) = (true, false, 1)).is_none(), "not a hole-free smaller into a bigger that is not");
        let (a, b) = (item(&s, 0, 0.0, 0.0, 0.3), item(&s, 1, 0.59, 0.0, 0.3));
        assert!(absorb(&c, &a, &b).unwrap().is_none(), "a 10 µm overlap does not survive the 25 µm erosion");
        let m = pair(&|a, b| {
            (a.depth, a.ramp, a.hold, a.roof_discs, a.under_roof, a.highest_tip) = (9, 3, 0, 2, false, 20);
            (b.depth, b.ramp, b.hold, b.roof_discs, b.under_roof, b.highest_tip) = (4, 4, 1, 1, true, 12);
        })
        .unwrap();
        let e = &m.made.st.e;
        assert_eq!((e.depth, e.ramp, e.hold, e.roof_discs, e.under_roof, e.highest_tip), (9, 4, 1, 1, true, 20), "larger counters and hold, smaller discs, either roof, the higher tip");
        assert!((e.collision_radius(&s) - s.radius(4, 0.0)).abs() < 1e-12, "the radius is the larger one's");
        assert_eq!(m.carries, vec![0, 1], "the absorber's carries first");
        // The smaller by collision radius (a, 0.76 mm) grown by the drawn radii's difference,
        // 0.88 − 0.76 mm (round, simplified at 0.015 mm), over the bigger's part.
        let (a, b) = (item(&s, 0, 0.0, 0.0, 0.3), item(&s, 1, 0.4, 0.0, 0.3));
        let grown = simplified(&offset(&a.made.st.to_bed, 0.12 * SCALE, Join::Round(0.01 * SCALE)), 0.015);
        let want = total_area_mm2(&intersection_ex(&grown, &b.made.st.to_bed));
        let got = total_area_mm2(&m.made.st.to_bed);
        assert!((got - want).abs() < 1e-9, "the to-bed part is the overlap: {got} mm², want {want}");
        assert!(got > total_area_mm2(&intersection_ex(&a.made.st.to_bed, &b.made.st.to_bed)) + 0.03, "grown by the difference");
    }
}
