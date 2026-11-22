//! A group is the pins, runs, junctions and tags of one net joined by its sheet wires.
//! Groups keep each wired piece together while its parts travel between layouts.

use crate::graph::Pt;
use crate::wiretree::on_stroke;
use crate::SheetLayout;
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub net: u32,
    /// Indices into the caller's `pins`, ascending.
    pub pins: Vec<u32>,
    /// Indices into `SheetLayout::wires`, ascending.
    pub runs: Vec<u32>,
    /// Indices into `SheetLayout::junctions`, ascending.
    pub junctions: Vec<u32>,
    /// Indices into `SheetLayout::tags`, ascending.
    pub tags: Vec<u32>,
}

#[derive(Clone, Copy)]
enum Item {
    Pin(u32),
    Run(u32),
    Junction(u32),
    Tag(u32),
}

struct Sets {
    parent: Vec<usize>,
    size: Vec<usize>,
}

impl Sets {
    fn new(len: usize) -> Self {
        Self { parent: (0..len).collect(), size: vec![1; len] }
    }

    fn root(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }

    fn join(&mut self, a: usize, b: usize) {
        let (mut a, mut b) = (self.root(a), self.root(b));
        if a == b {
            return;
        }
        if self.size[a] < self.size[b] {
            std::mem::swap(&mut a, &mut b);
        }
        self.parent[b] = a;
        self.size[a] += self.size[b];
    }
}

/// `pins`: (net, grid point) of every pin on a placed part, in the caller's order.
///
/// Items are bucketed by net first, with a union-find per net; different nets never join.
/// Every vertex belongs to its run, and runs sharing any vertex join. A run's first or last
/// point on another run's axis-aligned stroke joins it (a T, endpoints inclusive).
/// Pins, junctions and tags join runs at vertices or on strokes, and each other at equal points.
/// Every pin, run and junction belongs to exactly one group, including isolated items.
/// A tag with `Some(net)` follows these rules on that net. A tag with `None` joins the first
/// pin by caller index at exactly its point, on any net. A tag touching nothing is omitted.
/// Groups are ordered by net, then their smallest pin index, else run index, else junction
/// index, else tag index, with that precedence. Member lists ascend; there are no empty groups.
/// A point map per net joins coincident items; each item point scans that net's strokes:
/// O(points x strokes) per net, plus bucketing and point-map work, with no limits or caps.
/// Only vectors and sorted collections determine output order, never hash iteration.
pub fn wire_groups(sheet: &SheetLayout, pins: &[(u32, Pt)]) -> Vec<Group> {
    let mut nets = BTreeMap::<u32, Vec<Item>>::new();
    let mut pin_nets = HashMap::new();
    // Insert in member precedence and index order, also keeping the first pin at each point.
    for (i, &(net, at)) in pins.iter().enumerate() {
        nets.entry(net).or_default().push(Item::Pin(i as u32));
        pin_nets.entry(at).or_insert(net);
    }
    for (i, run) in sheet.wires.iter().enumerate() {
        nets.entry(run.net).or_default().push(Item::Run(i as u32));
    }
    for (i, &(net, _)) in sheet.junctions.iter().enumerate() {
        nets.entry(net).or_default().push(Item::Junction(i as u32));
    }
    for (i, tag) in sheet.tags.iter().enumerate() {
        let Some(net) = tag.net.or_else(|| pin_nets.get(&tag.at).copied()) else { continue };
        nets.entry(net).or_default().push(Item::Tag(i as u32));
    }

    let mut out = Vec::new();
    for (net, items) in nets {
        let mut sets = Sets::new(items.len());
        let mut points = HashMap::new();
        let mut probes = Vec::new();
        let mut strokes = Vec::new();
        for (i, &item) in items.iter().enumerate() {
            let at = match item {
                Item::Run(run) => {
                    let pts = &sheet.wires[run as usize].pts;
                    for (v, &at) in pts.iter().enumerate() {
                        if let Some(other) = points.insert(at, i) {
                            sets.join(i, other);
                        }
                        if v == 0 || v + 1 == pts.len() {
                            probes.push((at, i));
                        }
                    }
                    for stroke in pts.windows(2) {
                        strokes.push((i, stroke[0], stroke[1]));
                    }
                    continue;
                }
                Item::Pin(pin) => pins[pin as usize].1,
                Item::Junction(junction) => sheet.junctions[junction as usize].1,
                Item::Tag(tag) => sheet.tags[tag as usize].at,
            };
            if let Some(other) = points.insert(at, i) {
                sets.join(i, other);
            }
            probes.push((at, i));
        }
        for (at, item) in probes {
            for &(run, a, b) in &strokes {
                if item != run && on_stroke(at, a, b) {
                    sets.join(item, run);
                }
            }
        }

        // The first member encountered gives each group its output position.
        let mut groups = vec![None; items.len()];
        for (i, item) in items.into_iter().enumerate() {
            let root = sets.root(i);
            let group = *groups[root].get_or_insert_with(|| {
                let index = out.len();
                out.push(Group { net, pins: Vec::new(), runs: Vec::new(), junctions: Vec::new(), tags: Vec::new() });
                index
            });
            match item {
                Item::Pin(pin) => out[group].pins.push(pin),
                Item::Run(run) => out[group].runs.push(run),
                Item::Junction(junction) => out[group].junctions.push(junction),
                Item::Tag(tag) => out[group].tags.push(tag),
            }
        }
    }
    out.retain(|g| !g.pins.is_empty() || !g.runs.is_empty() || !g.junctions.is_empty() || g.tags.len() > 1);
    out
}

/// Arcs that keep a group's parts joined in flight: a minimum spanning tree over the rest
/// positions `at`, using Euclidean distance. `node[i]` is point i's part; lengths must match.
/// Points of the same node join at zero cost and never produce an edge. Each returned [i, j]
/// has i < j, indexes `at`, and joins different nodes. Together with the zero-cost joins,
/// exactly (distinct nodes - 1) edges connect all points; empty or one-node input has no edges.
/// Prim over points takes O(k^2) time and O(k) storage, including the result. Ties choose the
/// lower point index, then the lower source index. Edges follow the order Prim adds them.
pub fn spanning(at: &[[f64; 2]], node: &[u32]) -> Vec<[u32; 2]> {
    assert_eq!(at.len(), node.len(), "spanning: every point must have a node");
    let k = at.len();
    if k == 0 || node.iter().all(|&n| n == node[0]) {
        return Vec::new();
    }
    let mut used = vec![false; k];
    let mut cost = vec![f64::INFINITY; k];
    let mut from = vec![usize::MAX; k];
    let mut edges = Vec::new();
    cost[0] = 0.0;
    loop {
        let mut next: Option<usize> = None;
        for i in 0..k {
            if !used[i] && next.is_none_or(|j| cost[i].total_cmp(&cost[j]).is_lt()) {
                next = Some(i);
            }
        }
        let Some(next) = next else { break };
        let prev = from[next];
        if prev != usize::MAX {
            edges.push([prev.min(next) as u32, prev.max(next) as u32]);
        }
        // Take every zero-cost join of this node before choosing another arc, even when
        // different nodes have coincident points. Each point relaxes the remaining points once.
        for i in 0..k {
            if used[i] || node[i] != node[next] {
                continue;
            }
            used[i] = true;
            for j in 0..k {
                if used[j] || node[j] == node[next] {
                    continue;
                }
                let distance = (at[i][0] - at[j][0]).hypot(at[i][1] - at[j][1]);
                let order = distance.total_cmp(&cost[j]);
                if from[j] == usize::MAX || order.is_lt() || (order.is_eq() && i < from[j]) {
                    cost[j] = distance;
                    from[j] = i;
                }
            }
        }
    }
    edges
}
