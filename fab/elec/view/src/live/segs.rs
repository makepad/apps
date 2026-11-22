//! Stable current-texture rows shared by every pane showing the same geometry.
//! Removed rows wait for the host to clear them before reuse; registered trees
//! resolve their design pins once per tree version and lowering generation.
//! Prepare registrations on the worker, then merge them on the UI thread.

use fab_elec_bench::ValueIndex;
use fab_elec_core::ids::{BoardId, NetId, NetRef, PinRef};
use crate::live::net::NetTree;
use std::cmp::Ordering;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// The geometry a segment belongs to, never a pane: every pane showing that sheet or board reads
/// the same rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TreeOwner {
    /// A schematic sheet's wiring: the unit and the sheet's index in it.
    Sheet(BoardId, u32),
    /// A board's copper (the board view and the 3D view).
    Copper(BoardId),
}

/// A segment's stable id in its geometry: a schematic wire's is the sorted pins of one side of
/// the cut (the lexicographically smaller side); a track's is its item id. Opaque to the allocator.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SegKey(
    /// The kind word followed by identity words; empty for a missing registration key.
    pub Box<[u64]>,
);

impl SegKey {
    /// The first word of every key says its kind, so keys of different kinds never collide.
    const CUT: u64 = 0;
    const LOOP: u64 = 1;
    const ITEM: u64 = 2;

    /// A bridge's: the pins of one side of the cut, each packed into one word (board, comp, pin
    /// ix), sorted.
    pub fn pins(pins: &[PinRef]) -> SegKey {
        SegKey::packed(SegKey::CUT, pins)
    }

    /// A loop segment's (its removal splits nothing): the pins at its two end nodes, packed and
    /// sorted as for `pins`.
    pub fn looped(pins: &[PinRef]) -> SegKey {
        SegKey::packed(SegKey::LOOP, pins)
    }

    /// From a geometry item's id.
    pub fn item(id: u64) -> SegKey {
        SegKey::items(&[id])
    }

    /// From a geometry item's id of several words (a board wire piece: its track, path segment
    /// and piece).
    pub fn items(words: &[u64]) -> SegKey {
        SegKey(std::iter::once(SegKey::ITEM).chain(words.iter().copied()).collect())
    }

    fn packed(kind: u64, pins: &[PinRef]) -> SegKey {
        let mut words: Vec<u64> = pins
            .iter()
            .map(|p| (u64::from(p.board.0) << 48) | (u64::from(p.comp.0) << 16) | u64::from(p.pin.0))
            .collect();
        words.sort_unstable();
        words.insert(0, kind);
        SegKey(words.into_boxed_slice())
    }
}

/// Structural keys parallel to `segs`, derived only from connections and pins.
///
/// A bridge splits its connected part in two when removed. Its key is [`SegKey::pins`] of
/// whichever side has the smaller key: with the shared kind word first, this is the
/// lexicographic order of the sorted packed pins, and an empty side is smallest. Segments of
/// an unbranched run between junctions or pins have the same cut and key, and by Kirchhoff's
/// current law the same current; the allocator's occurrence order can change invisibly on
/// relayout.
///
/// A loop segment splits nothing when removed, including a self-loop or an edge doubled by
/// a parallel edge. Its key is [`SegKey::looped`] of its end nodes' pins, counting a self-loop's
/// node once. Equal loop keys use the allocator's occurrence order, which a relayout may
/// change; wiring loops are rare. An end outside `0..nodes` gives `SegKey::looped(&[])`, and
/// pins outside that range are ignored.
///
/// One iterative low-link DFS covers all connected parts without recursion. Cost is
/// O(nodes + segs + pins log pins), plus O(pins of the part) per bridge for its two sides,
/// plus the loop keys' end pins: at most O(pins x bridges) for a part's pins, with output of
/// up to that size.
pub fn cut_keys(nodes: u32, segs: &[(u32, u32)], pins: &[(u32, PinRef)]) -> Vec<SegKey> {
    let count = nodes as usize;
    let mut adjacent = vec![Vec::new(); count];
    for (seg, &(a, b)) in segs.iter().enumerate() {
        if a < nodes && b < nodes {
            adjacent[a as usize].push((b as usize, seg));
            adjacent[b as usize].push((a as usize, seg));
        }
    }
    let mut node_pins = vec![Vec::new(); count];
    for &(node, pin) in pins {
        if node < nodes {
            node_pins[node as usize].push(pin);
        }
    }
    // PinRef's field order and widths match the packed words' order. Keep candidates sorted
    // so the key constructors' sort is linear, including when a node has repeated pins.
    for pins in &mut node_pins {
        pins.sort_unstable();
    }

    let mut keys = vec![None; segs.len()];
    let mut order = vec![usize::MAX; count];
    let mut low = vec![0; count];
    let mut end = vec![0; count];
    let mut parent: Vec<Option<(usize, usize)>> = vec![None; count];
    let mut preorder = Vec::new();
    let mut stack = Vec::new();
    let mut bridges = Vec::new();
    for root in 0..count {
        if order[root] != usize::MAX {
            continue;
        }
        let start = preorder.len();
        order[root] = start;
        low[root] = start;
        preorder.push(root);
        stack.push((root, 0_usize));
        bridges.clear();
        while let Some((node, next)) = stack.pop() {
            if let Some(&(other, seg)) = adjacent[node].get(next) {
                stack.push((node, next + 1));
                // Skip only the incoming segment, not all edges to the parent node.
                if parent[node].is_some_and(|(_, incoming)| incoming == seg) {
                    continue;
                }
                if order[other] == usize::MAX {
                    parent[other] = Some((node, seg));
                    order[other] = preorder.len();
                    low[other] = order[other];
                    preorder.push(other);
                    stack.push((other, 0));
                } else {
                    low[node] = low[node].min(order[other]);
                }
            } else {
                end[node] = preorder.len();
                if let Some((up, seg)) = parent[node] {
                    if low[node] > order[up] {
                        bridges.push((seg, node));
                    }
                    low[up] = low[up].min(low[node]);
                }
            }
        }

        let mut part_pins = Vec::new();
        for &node in &preorder[start..] {
            for &pin in &node_pins[node] {
                part_pins.push((pin, order[node]));
            }
        }
        part_pins.sort_unstable_by_key(|&(pin, _)| pin);
        for &(seg, child) in &bridges {
            // A child's subtree is contiguous in preorder. Filtering the sorted part keeps
            // the lower side sorted without walking all its pinless nodes for every bridge.
            let lower: Vec<_> = part_pins
                .iter()
                .filter(|&&(_, at)| order[child] <= at && at < end[child])
                .map(|&(pin, _)| pin)
                .collect();
            let mut upper = Vec::new();
            let mut removed = 0;
            for &(pin, _) in &part_pins {
                if lower.get(removed) == Some(&pin) {
                    removed += 1;
                } else {
                    upper.push(pin);
                }
            }
            keys[seg] = Some(SegKey::pins(&lower).min(SegKey::pins(&upper)));
        }
    }

    keys.into_iter()
        .enumerate()
        .map(|(seg, key)| {
            if let Some(key) = key {
                return key;
            }
            let (a, b) = segs[seg];
            if a >= nodes || b >= nodes {
                return SegKey::looped(&[]);
            }
            let (a, b) = (a as usize, b as usize);
            if a == b {
                return SegKey::looped(&node_pins[a]);
            }
            // Merge sorted end pins so loop-key construction stays linear in those pins.
            let (first, second) = (&node_pins[a], &node_pins[b]);
            let (mut i, mut j) = (0, 0);
            let mut ends = Vec::new();
            while i < first.len() && j < second.len() {
                if first[i] <= second[j] {
                    ends.push(first[i]);
                    i += 1;
                } else {
                    ends.push(second[j]);
                    j += 1;
                }
            }
            ends.extend_from_slice(&first[i..]);
            ends.extend_from_slice(&second[j..]);
            SegKey::looped(&ends)
        })
        .collect()
}

/// A registered tree resolved against one lowering: the `NetTree` with its pins as
/// `ValueIndex::pin` indices (the index into `LiveValues::branch_i`), and each segment's row,
/// parallel to `tree.segs`.
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    /// The tree with design pins resolved to lowering indices.
    pub tree: NetTree,
    /// Current-texture rows parallel to the tree's segments.
    pub rows: Vec<u32>,
}

/// One net's wire graph as a view hands it: segments with their keys, pins by design id.
#[derive(Clone, Debug, PartialEq)]
pub struct TreeIn {
    /// The design net this tree belongs to.
    pub net: NetRef,
    /// Node count; nodes are `0..nodes`.
    pub nodes: u32,
    /// Endpoints; current is positive from the first node to the second.
    pub segs: Vec<(u32, u32)>,
    /// Parallel to `segs`.
    pub keys: Vec<SegKey>,
    /// (node, pin) for every component pin on this tree.
    pub pins: Vec<(u32, PinRef)>,
    /// As `NetTree::ports`.
    pub ports: Vec<u32>,
    /// As `NetTree::conductance` (may be empty).
    pub conductance: Vec<f32>,
}

impl Default for TreeIn {
    fn default() -> Self {
        Self {
            net: NetRef { board: BoardId(0), net: NetId(0) },
            nodes: 0,
            segs: Vec::new(),
            keys: Vec::new(),
            pins: Vec::new(),
            ports: Vec::new(),
            conductance: Vec::new(),
        }
    }
}

type SegmentId = (NetRef, SegKey, usize);

/// A registration prepared off the UI thread by [`SegRows::prepare`];
/// [`SegRows::set_prepared`] only merges it.
#[derive(Clone, Debug)]
pub struct Prepared {
    trees: Vec<TreeIn>,
    ids: Vec<SegmentId>,
    order: Vec<u32>,
    hash: u64,
}

impl Prepared {
    /// The trees supplied when preparing this registration.
    pub fn trees(&self) -> &[TreeIn] {
        &self.trees
    }
}

#[derive(Default)]
struct OwnerState {
    trees: Vec<TreeIn>,
    hash: u64,
    // Ascending by segment id.
    rows: Vec<(SegmentId, u32)>,
    tree_rows: Vec<Vec<u32>>,
}

fn trees_hash(trees: &[TreeIn]) -> u64 {
    let mut hasher = DefaultHasher::new();
    trees.len().hash(&mut hasher);
    for tree in trees {
        tree.net.hash(&mut hasher);
        tree.nodes.hash(&mut hasher);
        tree.segs.hash(&mut hasher);
        tree.keys.hash(&mut hasher);
        tree.pins.hash(&mut hasher);
        tree.ports.hash(&mut hasher);
        tree.conductance.len().hash(&mut hasher);
        for value in &tree.conductance {
            value.to_bits().hash(&mut hasher);
        }
    }
    hasher.finish()
}

// Occurrences span all trees in a registration, including separate trees of the same net.
fn segment_ids(trees: &[TreeIn]) -> impl Iterator<Item = SegmentId> + '_ {
    let mut occurrences = BTreeMap::new();
    trees
        .iter()
        .flat_map(|tree| {
            (0..tree.segs.len()).map(move |i| {
                (tree.net, tree.keys.get(i).cloned().unwrap_or_else(|| SegKey(Box::new([]))))
            })
        })
        .map(move |(net, key)| {
            let occurrence = occurrences.entry((net, key.clone())).or_insert(0_usize);
            let id = (net, key, *occurrence);
            *occurrence += 1;
            id
        })
}

/// The allocator and the registered trees.
pub struct SegRows {
    owners: BTreeMap<TreeOwner, OwnerState>,
    free: VecDeque<u32>,
    freed: Vec<u32>,
    count: usize,
    version: u64,
    // Mutating the registered trees invalidates this cache, even if the version wraps.
    cache: Option<(u64, Arc<Vec<Resolved>>)>,
}

impl SegRows {
    /// Creates an allocator with no registered trees or allocated rows.
    pub fn new() -> SegRows {
        Self {
            owners: BTreeMap::new(),
            free: VecDeque::new(),
            freed: Vec::new(),
            count: 0,
            version: 0,
            cache: None,
        }
    }

    /// Prepares segment identities, their sorted order and the trees' hash on any thread.
    pub fn prepare(trees: Vec<TreeIn>) -> Prepared {
        let ids: Vec<_> = segment_ids(&trees).collect();
        let mut order: Vec<u32> = (0..ids.len()).map(|i| i as u32).collect();
        order.sort_unstable_by(|&a, &b| ids[a as usize].cmp(&ids[b as usize]));
        let hash = trees_hash(&trees);
        Prepared { trees, ids, order, hash }
    }

    /// Replaces an owner's trees. Rows are identified by (owner, net, key, occurrence), with
    /// occurrences counted across this call. Missing keys use the empty key. Surviving rows stay
    /// put; new segments take the oldest cleared row, then the next new row. Identical trees do
    /// not change the version or allocate rows. Returns rows parallel to each tree's segments.
    ///
    /// Cost on the calling thread is O(segments) key comparisons and no key clones; the
    /// identical case is one hash comparison plus one deep comparison.
    pub fn set_prepared(&mut self, owner: TreeOwner, prepared: Prepared) -> Vec<Vec<u32>> {
        if let Some(previous) = self.owners.get(&owner) {
            if previous.hash == prepared.hash && previous.trees == prepared.trees {
                return previous.tree_rows.clone();
            }
        }

        let previous = self.owners.remove(&owner).unwrap_or_default();
        let Prepared { trees, ids, order, hash } = prepared;
        let mut tree_order_rows = vec![u32::MAX; ids.len()];
        let mut previous_rows = previous.rows.into_iter().peekable();
        for &position in &order {
            let position = position as usize;
            while let Some((id, row)) = previous_rows.peek() {
                match id.cmp(&ids[position]) {
                    Ordering::Less => {
                        self.freed.push(*row);
                        previous_rows.next();
                    }
                    Ordering::Equal => {
                        tree_order_rows[position] = *row;
                        previous_rows.next();
                        break;
                    }
                    Ordering::Greater => break,
                }
            }
        }
        self.freed.extend(previous_rows.map(|(_, row)| row));

        // Pending clears cannot be consumed by this or any subsequent set until take_freed.
        for row in &mut tree_order_rows {
            if *row == u32::MAX {
                *row = self.free.pop_front().unwrap_or_else(|| {
                    let row = self.count as u32;
                    self.count += 1;
                    row
                });
            }
        }

        let mut ids: Vec<_> = ids.into_iter().map(Some).collect();
        let rows = order
            .into_iter()
            .map(|position| {
                let position = position as usize;
                (ids[position].take().expect("each segment id occurs once in order"), tree_order_rows[position])
            })
            .collect();
        let mut offset = 0;
        let tree_rows: Vec<Vec<u32>> = trees
            .iter()
            .map(|tree| {
                let end = offset + tree.segs.len();
                let rows = tree_order_rows[offset..end].to_vec();
                offset = end;
                rows
            })
            .collect();
        let result = tree_rows.clone();
        let state = OwnerState { trees, hash, rows, tree_rows };
        self.owners.insert(owner, state);
        self.changed();
        result
    }

    /// `prepare` and `set_prepared` in one call, for callers that build their trees on the UI thread.
    pub fn set(&mut self, owner: TreeOwner, trees: Vec<TreeIn>) -> Vec<Vec<u32>> {
        self.set_prepared(owner, SegRows::prepare(trees))
    }

    /// Drops the owner's trees (the geometry is gone): its rows are freed.
    pub fn remove(&mut self, owner: TreeOwner) {
        if let Some(state) = self.owners.remove(&owner) {
            self.freed.extend(state.rows.into_iter().map(|(_, row)| row));
            self.changed();
        }
    }

    /// Rows freed since the last call, in the order freed, for the host to clear once. Only
    /// handing them out here makes them available to later registrations, in FIFO order.
    pub fn take_freed(&mut self) -> Vec<u32> {
        let freed = std::mem::take(&mut self.freed);
        self.free.extend(freed.iter().copied());
        freed
    }

    /// Rows ever made (in use plus free, including pending clears). Never shrinks.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether no texture rows have ever been allocated.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Rows in use now.
    pub fn in_use(&self) -> usize {
        self.count - self.free.len() - self.freed.len()
    }

    /// Bumps on every `set`, `set_prepared` or `remove` that changes anything.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// The registered owners, in owner order.
    pub fn owners(&self) -> impl Iterator<Item = TreeOwner> + '_ {
        self.owners.keys().copied()
    }

    /// The owner's rows in use, ascending.
    pub fn rows_of(&self, owner: TreeOwner) -> Vec<u32> {
        let mut rows: Vec<u32> = self.owners.get(&owner).into_iter().flat_map(|state| state.rows.iter().map(|&(_, row)| row)).collect();
        rows.sort_unstable();
        rows
    }

    /// Resolves every registered tree in owner order and then call order. Pins absent from this
    /// lowering are dropped. Reuses the same `Arc` until the trees or index generation change.
    pub fn resolved(&mut self, index: &ValueIndex) -> Arc<Vec<Resolved>> {
        if let Some((generation, resolved)) = &self.cache {
            if *generation == index.generation {
                return Arc::clone(resolved);
            }
        }
        let mut resolved = Vec::new();
        for state in self.owners.values() {
            for (tree, rows) in state.trees.iter().zip(&state.tree_rows) {
                resolved.push(Resolved {
                    tree: NetTree {
                        nodes: tree.nodes,
                        segs: tree.segs.clone(),
                        pins: tree.pins.iter().filter_map(|&(node, pin)| index.pin(pin).map(|i| (node, i as u32))).collect(),
                        ports: tree.ports.clone(),
                        conductance: tree.conductance.clone(),
                        ..NetTree::default()
                    },
                    rows: rows.clone(),
                });
            }
        }
        let resolved = Arc::new(resolved);
        self.cache = Some((index.generation, Arc::clone(&resolved)));
        resolved
    }

    fn changed(&mut self) {
        self.version = self.version.wrapping_add(1);
        self.cache = None;
    }
}

impl Default for SegRows {
    fn default() -> Self {
        Self::new()
    }
}
