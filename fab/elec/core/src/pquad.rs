//! A persistent loose quadtree over the whole integer nanometre plane.
//!
//! A box lives in the cell containing its centre at the deepest depth, up to 30, whose side
//! is at least twice its largest dimension. Larger boxes stay at the root. Each cell's loose
//! bounds grow by half its side on every edge. Sorted entries and pruning empty branches make
//! the structure depend only on the entry set, independent of insertion and removal history.
//!
//! Cloning costs O(1), sharing one root `Arc`. Insert/remove cost O(depth + node entries),
//! counting entries in shared path nodes that must be copied as well as the edited node.
//! `Arc::make_mut` copies only shared nodes on the edit path; other snapshots never change.
//! Query costs O(visited nodes + reported), plus testing entries in those visited nodes;
//! loose bounds prune subtrees, but a visited node can contain nonmatching entries.

use crate::geom::{BBox, Nm};
use std::fmt;
use std::ops::ControlFlow;
use std::sync::Arc;

const MAX_DEPTH: usize = 30;
const ROOT_SIDE: i64 = 1_i64 << 32;

#[derive(Clone, PartialEq)]
struct Node<T> {
    entries: Vec<(BBox, T)>,
    kids: [Option<Arc<Node<T>>>; 4],
}

impl<T> Node<T> {
    fn new() -> Self {
        Self { entries: Vec::new(), kids: [None, None, None, None] }
    }
}

#[derive(Clone, Copy)]
struct Cell {
    x: i64,
    y: i64,
    side: i64,
}

impl Cell {
    const ROOT: Self = Self { x: Nm::MIN as i64, y: Nm::MIN as i64, side: ROOT_SIDE };

    fn child(self, index: usize) -> Self {
        let side = self.side / 2;
        Self {
            x: self.x + (index % 2) as i64 * side,
            y: self.y + (index / 2) as i64 * side,
            side,
        }
    }

    fn meets(self, area: &BBox) -> bool {
        let margin = self.side / 2;
        // A cell covers side integer coordinates; keep its loose bounds in i64 without clipping.
        self.x - margin <= area.max.x as i64
            && area.min.x as i64 <= self.x + self.side - 1 + margin
            && self.y - margin <= area.max.y as i64
            && area.min.y as i64 <= self.y + self.side - 1 + margin
    }
}

/// A spatial snapshot of unique ordered values and their boxes.
///
/// Replacing a value requires the same geometric placement. To move it, remove its old box
/// first, then insert its new box. No global value lookup is maintained.
#[derive(Clone)]
pub struct PQuad<T> {
    root: Option<Arc<Node<T>>>,
    len: usize,
}

impl<T> Default for PQuad<T> {
    fn default() -> Self {
        Self { root: None, len: 0 }
    }
}

impl<T: PartialEq> PartialEq for PQuad<T> {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.root == other.root
    }
}

impl<T: fmt::Debug> fmt::Debug for PQuad<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PQuad").field("len", &self.len).finish()
    }
}

impl<T: Ord + Clone> PQuad<T> {
    /// Creates an empty index without allocating.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Inserts or replaces a value at the same placement, returning its previous box.
    ///
    /// Callers remove with the old box before inserting a value at a different placement.
    /// Panics if `bbox` is empty.
    pub fn insert(&mut self, bbox: BBox, value: T) -> Option<BBox> {
        assert!(!bbox.is_empty(), "PQuad cannot insert empty bbox: {bbox:?}");
        let (path, depth) = placement(bbox);
        let mut node = Arc::make_mut(self.root.get_or_insert_with(|| Arc::new(Node::new())));
        for &child in &path[..depth] {
            node = Arc::make_mut(node.kids[child].get_or_insert_with(|| Arc::new(Node::new())));
        }
        match node.entries.binary_search_by(|entry| entry.1.cmp(&value)) {
            Ok(index) => Some(std::mem::replace(&mut node.entries[index], (bbox, value)).0),
            Err(index) => {
                node.entries.insert(index, (bbox, value));
                self.len += 1;
                None
            }
        }
    }

    /// Removes a value with its exact stored box, returning whether it was present.
    pub fn remove(&mut self, bbox: BBox, value: &T) -> bool {
        if bbox.is_empty() {
            return false;
        }
        let (path, depth) = placement(bbox);
        if remove_at(&mut self.root, &path[..depth], bbox, value) {
            self.len -= 1;
            true
        } else {
            false
        }
    }

    /// Visits intersecting or touching boxes, entries first, then children in index order.
    ///
    /// Entries within each node are ordered by value. `Break(())` stops immediately.
    /// An empty query area has no hits.
    pub fn query(&self, area: BBox, mut f: impl FnMut(&BBox, &T) -> ControlFlow<()>) {
        if !area.is_empty() {
            if let Some(root) = self.root.as_deref() {
                let _ = query_node(root, Cell::ROOT, &area, &mut f);
            }
        }
    }

    /// Visits every entry in the same depth-first order as `query`, using O(depth) space.
    pub fn iter(&self) -> impl Iterator<Item = (&BBox, &T)> + '_ {
        let mut stack: Vec<_> = self.root.as_deref().into_iter().collect();
        let mut entries = [].iter();
        std::iter::from_fn(move || loop {
            if let Some((bbox, value)) = entries.next() {
                return Some((bbox, value));
            }
            let node = stack.pop()?;
            stack.extend(node.kids.iter().rev().filter_map(Option::as_deref));
            entries = node.entries.iter();
        })
    }

    #[cfg(test)]
    fn shares_child(&self, other: &Self, child: usize) -> bool {
        let (Some(left), Some(right)) = (&self.root, &other.root) else {
            return false;
        };
        match (&left.kids[child], &right.kids[child]) {
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }
}

fn placement(bbox: BBox) -> ([usize; MAX_DEPTH], usize) {
    let extent = bbox.width().max(bbox.height());
    let center = bbox.center();
    let x = center.x as i64 - Nm::MIN as i64;
    let y = center.y as i64 - Nm::MIN as i64;
    let mut path = [0; MAX_DEPTH];
    let mut depth = 0;
    let mut side = ROOT_SIDE;
    // A child accepts the box when its half-side (the parent's quarter-side) is large enough.
    while depth < MAX_DEPTH && extent <= side / 4 {
        let shift = 31 - depth;
        path[depth] = (2 * ((y >> shift) & 1) + ((x >> shift) & 1)) as usize;
        side /= 2;
        depth += 1;
    }
    (path, depth)
}

fn remove_at<T: Ord + Clone>(
    slot: &mut Option<Arc<Node<T>>>,
    path: &[usize],
    bbox: BBox,
    value: &T,
) -> bool {
    let Some(shared) = slot.as_mut() else {
        return false;
    };
    let node = Arc::make_mut(shared);
    let removed = if let Some((&child, rest)) = path.split_first() {
        remove_at(&mut node.kids[child], rest, bbox, value)
    } else {
        match node.entries.binary_search_by(|entry| entry.1.cmp(value)) {
            Ok(index) if node.entries[index].0 == bbox => {
                node.entries.remove(index);
                true
            }
            _ => false,
        }
    };
    if removed && node.entries.is_empty() && node.kids.iter().all(Option::is_none) {
        *slot = None;
    }
    removed
}

fn query_node<T>(
    node: &Node<T>,
    cell: Cell,
    area: &BBox,
    f: &mut impl FnMut(&BBox, &T) -> ControlFlow<()>,
) -> ControlFlow<()> {
    if !cell.meets(area) {
        return ControlFlow::Continue(());
    }
    for (bbox, value) in &node.entries {
        if bbox.intersects(area) {
            f(bbox, value)?;
        }
    }
    for (index, child) in node.kids.iter().enumerate() {
        if let Some(child) = child.as_deref() {
            query_node(child, cell.child(index), area, f)?;
        }
    }
    ControlFlow::Continue(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::P;

    fn random(state: &mut u64) -> u32 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        (*state >> 32) as u32
    }

    fn random_box(state: &mut u64) -> BBox {
        let kind = random(state) % 16;
        let limit = if kind == 0 { 1 << 30 } else { 2_000_000 };
        let width = if kind == 1 { 0 } else { random(state) % (limit + 1) } as i64;
        let height = (random(state) % (limit + 1)) as i64;
        let (x, y) = if random(state) % 4 == 0 {
            (random(state) as Nm as i64, random(state) as Nm as i64)
        } else {
            (
                (random(state) % 40_000_001) as i64 - 20_000_000,
                (random(state) % 40_000_001) as i64 - 20_000_000,
            )
        };
        let x = x.min(Nm::MAX as i64 - width);
        let y = y.min(Nm::MAX as i64 - height);
        BBox::new(P::new(x as Nm, y as Nm), P::new((x + width) as Nm, (y + height) as Nm))
    }

    fn plane() -> BBox {
        BBox::new(P::new(Nm::MIN, Nm::MIN), P::new(Nm::MAX, Nm::MAX))
    }

    fn hits(tree: &PQuad<u32>, area: BBox) -> Vec<u32> {
        let mut values = Vec::new();
        tree.query(area, |_, value| {
            values.push(*value);
            ControlFlow::Continue(())
        });
        values.sort_unstable();
        values
    }

    fn check_query(tree: &PQuad<u32>, model: &[(BBox, u32)], area: BBox) {
        let mut expected: Vec<_> = model.iter()
            .filter(|entry| entry.0.intersects(&area))
            .map(|entry| entry.1)
            .collect();
        expected.sort_unstable();
        assert_eq!(hits(tree, area), expected, "query equals brute force: {area:?}");
    }

    #[test]
    fn model_agrees() {
        let mut state = 0x6a09_e667_f3bc_c909;
        let mut tree = PQuad::new();
        let mut model = Vec::new();
        assert!(tree.is_empty(), "a new index is empty");
        assert_eq!(tree, PQuad::default(), "new and default have the same empty structure");
        let edges = [
            plane(),
            BBox::new(P::new(Nm::MIN, Nm::MIN), P::new(Nm::MIN + 1, Nm::MIN + 2)),
            BBox::new(P::new(Nm::MAX - 1, Nm::MAX - 2), P::new(Nm::MAX, Nm::MAX)),
            BBox::point(P::ZERO),
            BBox::new(P::new(-3, -3), P::new(-1, 1)),
            BBox::new(P::new(3, 0), P::new(5, 0)),
        ];
        for (value, bbox) in edges.into_iter().enumerate() {
            assert_eq!(tree.insert(bbox, value as u32), None, "a unique value inserts once");
            model.push((bbox, value as u32));
        }
        assert_eq!(
            tree.root.as_deref().map(|node| node.entries.as_slice()),
            Some(&[(plane(), 0)][..]),
            "the whole plane is placed at the root"
        );
        for &(bbox, _) in &model {
            check_query(&tree, &model, BBox::point(bbox.min));
            check_query(&tree, &model, BBox::point(bbox.max));
        }
        assert!(hits(&tree, BBox::EMPTY).is_empty(), "an empty query reports nothing");
        assert!(!tree.remove(BBox::point(P::ZERO), &u32::MAX), "removing an absent value returns false");
        assert!(!tree.remove(BBox::EMPTY, &0), "an empty box cannot remove an entry");

        let old = BBox::new(P::new(8, 8), P::new(9, 9));
        let replacement = BBox::new(P::new(8, 8), P::new(10, 10));
        let mut next = model.len() as u32;
        assert_eq!(tree.insert(old, next), None, "replacement setup inserts a unique value");
        assert_eq!(tree.insert(replacement, next), Some(old), "replacement returns the previous box");
        assert!(!tree.remove(old, &next), "removal requires the exact stored box");
        model.push((replacement, next));
        next += 1;
        assert_eq!(tree.len(), model.len(), "replacement preserves the entry count");

        for operation in 0..3_000 {
            let action = random(&mut state) % 5;
            if model.is_empty() || action < 2 {
                let bbox = random_box(&mut state);
                assert_eq!(tree.insert(bbox, next), None, "a new random value has no previous box");
                model.push((bbox, next));
                next += 1;
            } else {
                let index = random(&mut state) as usize % model.len();
                let (bbox, value) = model[index];
                match action {
                    2 => {
                        assert!(tree.remove(bbox, &value), "removing a present random value succeeds");
                        model.swap_remove(index);
                    }
                    3 => {
                        assert!(tree.remove(bbox, &value), "moving removes the old random box");
                        let moved = random_box(&mut state);
                        assert_eq!(tree.insert(moved, value), None, "moving inserts a new placement");
                        model[index].0 = moved;
                    }
                    _ => {
                        assert_eq!(tree.insert(bbox, value), Some(bbox), "replacing never duplicates a value");
                    }
                }
            }
            if (operation + 1) % 100 == 0 {
                assert_eq!(tree.len(), model.len(), "query equals brute force: entry count");
                for _ in 0..20 {
                    check_query(&tree, &model, random_box(&mut state));
                }
            }
        }
        check_query(&tree, &model, plane());
        for (bbox, value) in model {
            assert!(tree.remove(bbox, &value), "draining removes every surviving entry");
        }
        assert!(tree.is_empty(), "draining clears the entry count");
        assert_eq!(tree, PQuad::new(), "draining prunes every empty branch including the root");
        assert_eq!(tree.iter().count(), 0, "empty iteration yields no entries");
    }

    #[test]
    fn history_independent() {
        let mut state = 0xbb67_ae85_84ca_a73b;
        let entries: Vec<_> = (0..1_000_u32).map(|value| (random_box(&mut state), value)).collect();
        let mut forward = PQuad::new();
        for &(bbox, value) in &entries {
            assert_eq!(forward.insert(bbox, value), None, "forward history contains unique values");
        }
        let mut shuffled = entries.clone();
        shuffled.extend((1_000..1_300).map(|value| (random_box(&mut state), value)));
        for index in (1..shuffled.len()).rev() {
            let other = random(&mut state) as usize % (index + 1);
            shuffled.swap(index, other);
        }
        let mut edited = PQuad::new();
        for &(bbox, value) in &shuffled {
            assert_eq!(edited.insert(bbox, value), None, "shuffled history contains unique values");
        }
        for &(bbox, value) in &shuffled {
            if value >= 1_000 {
                assert!(edited.remove(bbox, &value), "extra history entries are removable");
            }
        }
        assert_eq!(forward, edited, "shape depends on the set only");
        let expected: Vec<_> = forward.iter().map(|(bbox, value)| (*bbox, *value)).collect();
        let actual: Vec<_> = edited.iter().map(|(bbox, value)| (*bbox, *value)).collect();
        assert_eq!(actual, expected, "shape depends on the set only: iteration order");
        let mut queried = Vec::new();
        edited.query(plane(), |bbox, value| {
            queried.push((*bbox, *value));
            ControlFlow::Continue(())
        });
        assert_eq!(queried, expected, "query and iteration have the same deterministic order");
        let mut prefix = Vec::new();
        edited.query(plane(), |bbox, value| {
            prefix.push((*bbox, *value));
            if prefix.len() == 7 { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        });
        assert_eq!(prefix, expected[..7], "Break stops traversal immediately across node boundaries");
    }

    #[test]
    fn persistent() {
        let mut tree = PQuad::new();
        let mut original = Vec::new();
        for value in 0..400_u32 {
            let sx = if value & 1 == 0 { -1 } else { 1 };
            let sy = if value & 2 == 0 { -1 } else { 1 };
            let x = sx * (100_000_000 + (value / 4) as Nm * 10_000);
            let y = sy * (100_000_000 + (value / 4 * 19 % 101) as Nm * 10_000);
            let bbox = BBox::new(P::new(x, y), P::new(x + 1_000, y + 1_000));
            assert_eq!(tree.insert(bbox, value), None, "snapshot setup contains unique values");
            original.push((bbox, value));
        }
        assert_eq!(tree.insert(plane(), 400), None, "snapshot setup includes a root entry");
        original.push((plane(), 400));
        let a = tree.clone();
        assert!(
            Arc::ptr_eq(a.root.as_ref().unwrap(), tree.root.as_ref().unwrap()),
            "clone shares the root in constant time"
        );
        let before_iter: Vec<_> = a.iter().map(|(bbox, value)| (*bbox, *value)).collect();
        let mut state = 0x3c6e_f372_fe94_f82b;
        let mut areas = vec![plane(), BBox::EMPTY];
        areas.extend(original.iter().flat_map(|entry| [entry.0, BBox::point(entry.0.max)]));
        areas.extend((0..40).map(|_| random_box(&mut state)));
        let before: Vec<_> = areas.iter().map(|area| hits(&a, *area)).collect();

        // Only the lower-left quadrant changes; all other root children must stay shared.
        for edit in 0..200_u32 {
            if edit % 2 == 0 {
                let (bbox, value) = original[(edit / 2 * 4) as usize];
                assert!(tree.remove(bbox, &value), "persistent edits remove a lower-left entry");
            } else {
                let x = -200_000_000 + edit as Nm * 10_000;
                let bbox = BBox::new(P::new(x, -200_000_000), P::new(x + 1_000, -199_999_000));
                assert_eq!(tree.insert(bbox, 400 + edit), None, "persistent edits insert a unique value");
            }
        }
        for (area, expected) in areas.into_iter().zip(before) {
            assert_eq!(hits(&a, area), expected, "a snapshot never changes: {area:?}");
        }
        assert_eq!(a.len(), original.len(), "a snapshot never changes: entry count");
        assert_eq!(
            a.iter().map(|(bbox, value)| (*bbox, *value)).collect::<Vec<_>>(),
            before_iter,
            "a snapshot never changes: iteration"
        );
        assert!((1..4).all(|child| a.shares_child(&tree, child)), "edits copy only their paths");
        assert!(!a.shares_child(&tree, 0), "the edited subtree has its own path nodes");
        assert_ne!(a, tree, "edits change the current tree while preserving its snapshot");
    }
}
