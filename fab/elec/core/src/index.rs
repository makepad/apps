//! A flat, STR-packed 2D R-tree with static tombstones and a small dynamic edit buffer.
//!
//! Let `n` be the static entry count (including tombstones), `d` the dynamic count, `k` the
//! number of hits, and `v` the nodes and entries visited by a search. `new`, `default`, `len`,
//! and `is_empty` cost O(1). `build` costs O(m log m) for m input items, with O(m) space;
//! `clone` costs O(n + d). Insertion costs amortised O(log(n + d)) including rebuilds.
//! Removal costs O(v + d) to locate an item, plus amortised O(log(n + d)) for rebuilds.
//! `query` and `query_vec` typically cost O(log n + k + d), but overlapping boxes can make
//! them O(n + d); collecting hits uses O(k) extra space. `nearest` costs O(v log(v + 1) + d)
//! and O(v) extra space, excluding caller distance calculations. Creating `iter` costs O(1),
//! consuming it costs O(n + d), and `bbox` costs O(n + d). A rebuild costs O((n + d) log(n + d))
//! on the edit that triggers it. Tree traversals are iterative; query/removal stacks use O(log n)
//! space with the fixed fan-out of 16. Empty boxes are never stored.

use crate::geom::{BBox, Nm, P};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ops::{ControlFlow, Range};

const FANOUT: usize = 16;

#[derive(Clone)]
struct Node {
    bbox: BBox,
    range: Range<usize>,
    leaf: bool,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Visit {
    Node(usize),
    Entry(usize),
}

/// A spatial index of boxes and copyable values, allowing duplicate boxes and values.
///
/// Queries combine the alive static entries with dynamic edits. Cloning makes an independent
/// copy of the complete index, including tombstones and edits; `Default` creates an empty index.
#[derive(Clone)]
pub struct Index<T: Copy + PartialEq> {
    nodes: Vec<Node>,
    children: Vec<usize>,
    entries: Vec<(BBox, T)>,
    alive: Vec<bool>,
    static_alive: usize,
    root: Option<usize>,
    dynamic: Vec<(BBox, T)>,
}

impl<T: Copy + PartialEq> Default for Index<T> {
    /// Creates an empty index without allocating.
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + PartialEq> Index<T> {
    /// Creates an empty index without allocating.
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            children: Vec::new(),
            entries: Vec::new(),
            alive: Vec::new(),
            static_alive: 0,
            root: None,
            dynamic: Vec::new(),
        }
    }

    /// Bulk loads using Sort-Tile-Recursive packing. Empty boxes are skipped.
    pub fn build(mut items: Vec<(BBox, T)>) -> Self {
        items.retain(|(b, _)| !b.is_empty());
        str_order(&mut items, |entry| entry.0);

        let mut index = Self::new();
        index.static_alive = items.len();
        index.alive = vec![true; items.len()];
        index.entries = items;

        let mut level = Vec::with_capacity(index.entries.len().div_ceil(FANOUT));
        for (group, entries) in index.entries.chunks(FANOUT).enumerate() {
            let start = group * FANOUT;
            level.push(index.nodes.len());
            index.nodes.push(Node {
                bbox: entries.iter().fold(BBox::EMPTY, |b, entry| b.union(entry.0)),
                range: start..start + entries.len(),
                leaf: true,
            });
        }

        while level.len() > 1 {
            str_order(&mut level, |id| index.nodes[*id].bbox);
            let mut parents = Vec::with_capacity(level.len().div_ceil(FANOUT));
            for group in level.chunks(FANOUT) {
                let bbox = group.iter().fold(BBox::EMPTY, |b, id| b.union(index.nodes[*id].bbox));
                let start = index.children.len();
                index.children.extend_from_slice(group);
                parents.push(index.nodes.len());
                index.nodes.push(Node {
                    bbox,
                    range: start..index.children.len(),
                    leaf: false,
                });
            }
            level = parents;
        }
        index.root = level.first().copied();
        index
    }

    /// Returns the number of alive items: static alive entries plus dynamic entries.
    pub fn len(&self) -> usize {
        self.static_alive + self.dynamic.len()
    }

    /// Returns whether the index has no alive items.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Adds an item to the dynamic part; empty boxes are skipped.
    ///
    /// When the dynamic count exceeds `max(64, static_alive / 8)`, or tombstones exceed
    /// `max(64, static_len / 4)`, all alive items are rebuilt using `build`. Rebuilding never
    /// drops an alive item and adds amortised O(log n) work per edit.
    pub fn insert(&mut self, b: BBox, t: T) {
        if b.is_empty() {
            return;
        }
        self.dynamic.push((b, t));
        self.rebuild_if_needed();
    }

    /// Removes one item equal to `t` whose stored box equals `b`, returning false if absent.
    ///
    /// Static entries become tombstones, found by descending only nodes whose box contains `b`.
    /// Node boxes are not shrunk; a successful removal checks the same rebuild thresholds as insertion.
    pub fn remove(&mut self, b: BBox, t: T) -> bool {
        if b.is_empty() {
            return false;
        }
        if let Some(id) = self.dynamic.iter().position(|entry| entry.0 == b && entry.1 == t) {
            self.dynamic.swap_remove(id);
            self.rebuild_if_needed();
            return true;
        }

        let mut stack = Vec::new();
        if let Some(root) = self.root {
            if self.nodes[root].bbox.contains_box(&b) {
                stack.push(root);
            }
        }
        while let Some(id) = stack.pop() {
            let node = &self.nodes[id];
            if node.leaf {
                for entry in node.range.clone() {
                    if self.alive[entry] && self.entries[entry].0 == b && self.entries[entry].1 == t {
                        self.alive[entry] = false;
                        self.static_alive -= 1;
                        self.rebuild_if_needed();
                        return true;
                    }
                }
            } else {
                for &child in &self.children[node.range.clone()] {
                    if self.nodes[child].bbox.contains_box(&b) {
                        stack.push(child);
                    }
                }
            }
        }
        false
    }

    /// Calls `f` for every alive item whose box intersects or touches `area`.
    ///
    /// Stops immediately when `f` returns `ControlFlow::Break(())`. An empty area has no hits.
    pub fn query(&self, area: BBox, mut f: impl FnMut(BBox, T) -> ControlFlow<()>) {
        if area.is_empty() {
            return;
        }
        let mut stack = Vec::new();
        if let Some(root) = self.root {
            if self.nodes[root].bbox.intersects(&area) {
                stack.push(root);
            }
        }
        while let Some(id) = stack.pop() {
            let node = &self.nodes[id];
            if node.leaf {
                for entry in node.range.clone() {
                    let (b, t) = self.entries[entry];
                    if self.alive[entry] && b.intersects(&area) && f(b, t).is_break() {
                        return;
                    }
                }
            } else {
                for &child in &self.children[node.range.clone()] {
                    if self.nodes[child].bbox.intersects(&area) {
                        stack.push(child);
                    }
                }
            }
        }
        for &(b, t) in &self.dynamic {
            if b.intersects(&area) && f(b, t).is_break() {
                return;
            }
        }
    }

    /// Collects the values returned by `query`, preserving duplicates and promising no order.
    pub fn query_vec(&self, area: BBox) -> Vec<T> {
        let mut hits = Vec::new();
        self.query(area, |_, t| {
            hits.push(t);
            ControlFlow::Continue(())
        });
        hits
    }

    /// Finds the nearest item to `p` within `max_dist` (inclusive), using a caller's exact distance.
    ///
    /// `dist(b, t)` returns the item's nonnegative distance in nm, at least its box distance,
    /// or `None` to skip it. Box bounds are rounded down conservatively for whole-nm results.
    /// Static entries are searched best-first by box distance; the dynamic part is scanned first.
    /// Returns the item and its distance, retaining the first found on ties. Negative limits and
    /// empty indices return `None` without invoking `dist`.
    pub fn nearest(
        &self,
        p: P,
        max_dist: Nm,
        mut dist: impl FnMut(BBox, T) -> Option<Nm>,
    ) -> Option<(T, Nm)> {
        if max_dist < 0 || self.is_empty() {
            return None;
        }
        let mut best: Option<(T, Nm)> = None;
        let can_improve = |distance2: i128, best: Option<(T, Nm)>| {
            let lower = (distance2 as f64).sqrt().floor();
            match best {
                Some((_, distance)) => lower < distance as f64,
                None => lower <= max_dist as f64,
            }
        };
        let mut consider = |b, t, best: &mut Option<(T, Nm)>| {
            if let Some(distance) = dist(b, t) {
                if distance >= 0 && distance <= max_dist && best.map_or(true, |(_, old)| distance < old) {
                    *best = Some((t, distance));
                }
            }
        };

        for &(b, t) in &self.dynamic {
            if can_improve(b.dist2_to(p), best) {
                consider(b, t, &mut best);
            }
        }

        let mut heap = BinaryHeap::new();
        if let Some(root) = self.root {
            let distance2 = self.nodes[root].bbox.dist2_to(p);
            if can_improve(distance2, best) {
                heap.push(Reverse((distance2, Visit::Node(root))));
            }
        }
        while let Some(Reverse((distance2, visit))) = heap.pop() {
            if !can_improve(distance2, best) {
                break;
            }
            match visit {
                Visit::Node(id) => {
                    let node = &self.nodes[id];
                    if node.leaf {
                        for entry in node.range.clone() {
                            if self.alive[entry] {
                                let distance2 = self.entries[entry].0.dist2_to(p);
                                if can_improve(distance2, best) {
                                    heap.push(Reverse((distance2, Visit::Entry(entry))));
                                }
                            }
                        }
                    } else {
                        for &child in &self.children[node.range.clone()] {
                            let distance2 = self.nodes[child].bbox.dist2_to(p);
                            if can_improve(distance2, best) {
                                heap.push(Reverse((distance2, Visit::Node(child))));
                            }
                        }
                    }
                }
                Visit::Entry(entry) => {
                    let (b, t) = self.entries[entry];
                    consider(b, t, &mut best);
                }
            }
        }
        best
    }

    /// Iterates over every alive item, static then dynamic, with no order promised within either part.
    pub fn iter(&self) -> impl Iterator<Item = (BBox, T)> + '_ {
        self.entries
            .iter()
            .zip(&self.alive)
            .filter_map(|(&entry, &alive)| alive.then_some(entry))
            .chain(self.dynamic.iter().copied())
    }

    /// Returns the union of all alive boxes, or `BBox::EMPTY` when empty.
    pub fn bbox(&self) -> BBox {
        self.iter().fold(BBox::EMPTY, |b, entry| b.union(entry.0))
    }

    fn rebuild_if_needed(&mut self) {
        let tombstones = self.entries.len() - self.static_alive;
        if self.dynamic.len() > 64.max(self.static_alive / 8) || tombstones > 64.max(self.entries.len() / 4) {
            *self = Self::build(self.iter().collect());
        }
    }
}

fn str_order<U>(items: &mut [U], bbox: impl Fn(&U) -> BBox) {
    if items.is_empty() {
        return;
    }
    let leaf_count = items.len().div_ceil(FANOUT);
    let slice_leaves = (leaf_count as f64).sqrt().ceil() as usize;
    // Twice the centre retains half-nm ordering and cannot overflow at either i32 extreme.
    items.sort_unstable_by_key(|item| {
        let b = bbox(item);
        b.min.x as i64 + b.max.x as i64
    });
    for slice in items.chunks_mut(slice_leaves * FANOUT) {
        slice.sort_unstable_by_key(|item| {
            let b = bbox(item);
            b.min.y as i64 + b.max.y as i64
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random(state: &mut u64) -> u32 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        (*state >> 32) as u32
    }

    fn random_box(state: &mut u64) -> BBox {
        let width = 1 + random(state) % 1_000_000;
        let height = 1 + random(state) % 1_000_000;
        let x = (random(state) % (100_000_001 - width)) as Nm;
        let y = (random(state) % (100_000_001 - height)) as Nm;
        BBox::new(P::new(x, y), P::new(x + width as Nm, y + height as Nm))
    }

    #[test]
    fn query_insert_remove_match_brute_force() {
        let mut state = 0x6a09_e667_f3bc_c909;
        let mut items: Vec<_> = (0..5_000_u32).map(|id| (random_box(&mut state), id)).collect();
        let mut index = Index::build(items.clone());
        assert_eq!(index.len(), 5_000, "bulk load preserves the item count");
        assert!(!index.is_empty(), "bulk load is nonempty");

        let check_queries = |index: &Index<u32>, items: &[(BBox, u32)], state: &mut u64| {
            for probe in 0..200 {
                let area = random_box(state);
                let mut expected: Vec<_> = items.iter().filter(|(b, _)| b.intersects(&area)).map(|x| x.1).collect();
                let mut actual = index.query_vec(area);
                expected.sort_unstable();
                actual.sort_unstable();
                assert_eq!(actual, expected, "query {probe} matches brute force for {} items", items.len());
            }
        };
        check_queries(&index, &items, &mut state);

        for id in 5_000..5_500 {
            let b = random_box(&mut state);
            index.insert(b, id);
            items.push((b, id));
        }
        assert_eq!(index.len(), 5_500, "dynamic inserts increase the item count");
        let mut alive = vec![true; items.len()];
        for removal in 0..700 {
            let id = if removal % 2 == 0 { removal / 2 } else { 5_000 + removal / 2 };
            let (b, t) = items[id];
            assert!(index.remove(b, t), "mixed static/dynamic removal finds item {t}");
            alive[id] = false;
        }
        items.retain(|(_, id)| alive[*id as usize]);
        assert_eq!(index.len(), 4_800, "removal updates the alive count");
        check_queries(&index, &items, &mut state);
        assert!(!index.remove(items[0].0, u32::MAX), "removing a missing value returns false");

        let expected_box = items.iter().fold(BBox::EMPTY, |b, item| b.union(item.0));
        assert_eq!(index.bbox(), expected_box, "bbox is the union of alive boxes");
        let mut visits = 0;
        index.query(expected_box, |_, _| {
            visits += 1;
            ControlFlow::Break(())
        });
        assert_eq!(visits, 1, "early Break stops after the first hit across both parts");
        assert!(index.query_vec(BBox::EMPTY).is_empty(), "an empty query has no hits");
        let mut cloned_items: Vec<_> = index.clone().iter().collect();
        cloned_items.sort_unstable_by_key(|item| item.1);
        assert_eq!(cloned_items, items, "clone and iteration preserve all alive items");

        // The large setup's 500 inserts and 700 removals cannot cross either rebuild threshold.
        // Reuse its boxes in a smaller index to exercise both thresholds without more random work.
        let mut rebuilding = Index::build(items[..256].to_vec());
        for &(b, t) in &items[256..320] {
            rebuilding.insert(b, t);
        }
        assert_eq!(rebuilding.dynamic.len(), 64, "dynamic threshold is exclusive");
        rebuilding.insert(items[320].0, items[320].1);
        assert!(rebuilding.dynamic.is_empty(), "the 65th dynamic insert triggers a rebuild");
        let mut rebuilt_items: Vec<_> = rebuilding.iter().collect();
        rebuilt_items.sort_unstable_by_key(|item| item.1);
        assert_eq!(rebuilt_items, items[..321], "insertion rebuild preserves every item");

        for &(b, t) in &items[321..353] {
            rebuilding.insert(b, t);
        }
        for &(b, t) in &items[..80] {
            assert!(rebuilding.remove(b, t), "static removal before tombstone rebuild succeeds");
        }
        assert_eq!(rebuilding.entries.len(), 321, "tombstone threshold is exclusive");
        assert!(rebuilding.remove(items[80].0, items[80].1), "threshold-crossing removal succeeds");
        assert!(rebuilding.dynamic.is_empty(), "tombstone rebuild absorbs dynamic survivors");
        let mut rebuilt_items: Vec<_> = rebuilding.iter().collect();
        rebuilt_items.sort_unstable_by_key(|item| item.1);
        assert_eq!(rebuilt_items, items[81..353], "tombstone rebuild preserves exactly the alive items");

        let (b, t) = items[100];
        rebuilding.insert(b, t);
        assert!(rebuilding.remove(b, t), "one duplicate can be removed");
        assert_eq!(rebuilding.iter().filter(|item| *item == (b, t)).count(), 1, "one duplicate survives");
        assert!(!rebuilding.remove(BBox::point(b.min), t), "removal requires an equal stored box");
        assert!(rebuilding.query_vec(BBox::point(b.max)).contains(&t), "queries include touching corners");
        for &(b, t) in &items[81..353] {
            assert!(rebuilding.remove(b, t), "draining the rebuilt index finds every survivor");
        }
        assert!(rebuilding.is_empty(), "draining leaves no alive entries");
        assert_eq!(rebuilding.bbox(), BBox::EMPTY, "tombstoned node boxes do not affect the empty bbox");
        assert!(rebuilding.query_vec(expected_box).is_empty(), "tombstones never appear in queries");

        let mut empty = Index::<u32>::default();
        empty.insert(BBox::EMPTY, 0);
        assert!(empty.is_empty(), "empty insertions are skipped");
        assert!(!empty.remove(BBox::EMPTY, 0), "empty boxes cannot be removed");
        assert_eq!(empty.iter().count(), 0, "empty iteration yields nothing");
        assert_eq!(empty.bbox(), BBox::EMPTY, "a new index has an empty bbox");
        assert!(Index::build(vec![(BBox::EMPTY, 0)]).is_empty(), "bulk load skips empty boxes");
    }

    #[test]
    fn nearest_matches_brute_force() {
        let mut state = 0xbb67_ae85_84ca_a73b;
        let items: Vec<_> = (0..2_000_u32)
            .map(|id| {
                let p = P::new((random(&mut state) % 100_000_000) as Nm, (random(&mut state) % 100_000_000) as Nm);
                (BBox::point(p), id)
            })
            .collect();
        let mut index = Index::build(items.clone());
        // Move some entries into the dynamic part, retaining static tombstones and the same oracle.
        for &(b, t) in &items[..32] {
            assert!(index.remove(b, t), "nearest setup removes a static point");
            index.insert(b, t);
        }
        for probe in 0..200 {
            let p = P::new((random(&mut state) % 100_000_000) as Nm, (random(&mut state) % 100_000_000) as Nm);
            let distance = |b: BBox| (b.min.dist2(p) as f64).sqrt() as Nm;
            let expected = items.iter().map(|item| distance(item.0)).filter(|d| *d <= 5_000_000).min();
            let actual = index.nearest(p, 5_000_000, |b, _| Some(distance(b)));
            assert_eq!(actual.map(|item| item.1), expected, "nearest probe {probe} matches brute-force distance");
            if let Some((id, d)) = actual {
                assert_eq!(distance(items[id as usize].0), d, "nearest probe {probe} returns the matching item");
            }
        }

        for id in [0, 32] {
            let at = items[id].0.min;
            let p = P::new(at.x + 1, at.y + 1);
            let actual = index.nearest(p, 1, |b, t| {
                (t == id as u32).then(|| (b.min.dist2(p) as f64).sqrt() as Nm)
            });
            assert_eq!(actual, Some((id as u32, 1)), "static and dynamic rounded boundary distances are inclusive");
        }
        assert_eq!(
            index.nearest(items[0].0.min, 0, |b, _| Some((b.min.dist2(items[0].0.min) as f64).sqrt() as Nm)),
            Some((0, 0)),
            "a coincident dynamic point is found at zero radius"
        );
        assert_eq!(index.nearest(P::ZERO, Nm::MAX, |_, _| None), None, "None skips every candidate");
        let far = P::new(Nm::MIN, Nm::MIN);
        assert_eq!(
            index.nearest(far, 0, |b, _| Some((b.min.dist2(far) as f64).sqrt() as Nm)),
            None,
            "a zero-radius probe far from all items has no nearest item"
        );
        assert_eq!(
            Index::<u32>::new().nearest(P::ZERO, Nm::MAX, |_, _| panic!("empty index must not call dist")),
            None,
            "the empty index has no nearest item"
        );
        assert_eq!(
            index.nearest(P::ZERO, -1, |_, _| panic!("negative radius must not call dist")),
            None,
            "a negative distance limit has no nearest item"
        );
    }
}
